//! Real-thread runtime: worker threads and the watchdog thread driving the step functions.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::backend::Backend;
use crate::shared::{Clock, Shared, Sink, StepResult, Worker};
use crate::sync::Sleepers;

pub struct RealClock(Instant);

impl RealClock {
    pub fn new() -> Self {
        RealClock(Instant::now())
    }
}

impl Default for RealClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for RealClock {
    fn now(&self) -> u64 {
        self.0.elapsed().as_nanos() as u64 + 1
    }
}

struct PoolState {
    stop: AtomicBool,
    target: usize,
    active: AtomicUsize,
    /// Workers allowed beyond `target`, from the last watchdog round.
    allowed_extra: AtomicUsize,
    idle_threads: Sleepers,
    handles: Mutex<Vec<JoinHandle<()>>>,
}

/// Worker threads plus a watchdog thread.
pub struct Pool {
    state: Arc<PoolState>,
    watchdog: Option<JoinHandle<()>>,
}

impl Pool {
    pub fn start<B: Backend, S: Sink + Send + 'static>(
        shared: Arc<Shared<B>>,
        sink: Arc<S>,
        workers: usize,
    ) -> Pool {
        let workers = workers.max(1);
        let state = Arc::new(PoolState {
            stop: AtomicBool::new(false),
            target: workers,
            active: AtomicUsize::new(0),
            allowed_extra: AtomicUsize::new(0),
            idle_threads: Sleepers::default(),
            handles: Mutex::new(Vec::new()),
        });
        let st = state.clone();
        shared.set_waker(Box::new(move || st.idle_threads.wake_all()));
        let clock = Arc::new(RealClock::new());
        for _ in 0..workers {
            spawn_worker(&state, &shared, &sink, &clock);
        }
        let (st, sh, sk, ck) = (state.clone(), shared.clone(), sink.clone(), clock.clone());
        let watchdog = thread::Builder::new()
            .name("dfdu-watchdog".into())
            .spawn(move || {
                while !st.stop.load(SeqCst) {
                    let rep = sh.watchdog_round(&*sk, ck.now());
                    let allowed = sh.allowed_extra_workers(st.target, &rep);
                    st.allowed_extra.store(allowed, SeqCst);
                    // Workers for limited mounts are added at once; replacements for stuck
                    // workers one per round.
                    let mut want = st.target + sh.mount_workers();
                    while st.active.load(SeqCst) < want {
                        spawn_worker(&st, &sh, &sk, &ck);
                    }
                    want += allowed;
                    if st.active.load(SeqCst) < want {
                        spawn_worker(&st, &sh, &sk, &ck);
                    }
                    thread::sleep(Duration::from_millis(10));
                }
            })
            .expect("spawn watchdog");
        Pool {
            state,
            watchdog: Some(watchdog),
        }
    }

    /// Stops all threads, waiting at most `wait` for the workers once the watchdog has stopped.
    /// Threads stuck in a filesystem call are left behind.
    pub fn stop(mut self, wait: Duration) {
        self.state.stop.store(true, SeqCst);
        self.state.idle_threads.wake_all();
        if let Some(w) = self.watchdog.take() {
            let _ = w.join();
        }
        let deadline = Instant::now() + wait;
        let handles: Vec<_> = self.state.handles.lock().drain(..).collect();
        for h in handles {
            while !h.is_finished() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(1));
            }
            if h.is_finished() {
                let _ = h.join();
            }
        }
    }
}

fn spawn_worker<B: Backend, S: Sink + Send + 'static>(
    state: &Arc<PoolState>,
    shared: &Arc<Shared<B>>,
    sink: &Arc<S>,
    clock: &Arc<RealClock>,
) {
    let (st, sh, sk, ck) = (state.clone(), shared.clone(), sink.clone(), clock.clone());
    st.active.fetch_add(1, SeqCst);
    let h = thread::Builder::new()
        .name("dfdu-worker".into())
        .spawn(move || {
            let mut w = Worker::new(&sh, true);
            let mut retired = false;
            loop {
                if st.stop.load(SeqCst) {
                    break;
                }
                // A replacement worker made the pool larger than allowed: a worker without a
                // task leaves. The compare-and-swap lets only as many leave as are surplus. A
                // worker holding a handed-over task stays to run it.
                if !w.is_running() {
                    let limit = st.target + sh.mount_workers() + st.allowed_extra.load(SeqCst);
                    let a = st.active.load(SeqCst);
                    if a > limit
                        && !w.has_handoff()
                        && st.active.compare_exchange(a, a - 1, SeqCst, SeqCst).is_ok()
                    {
                        retired = true;
                        break;
                    }
                }
                match w.step(&sh, &*sk, &*ck) {
                    StepResult::Progress => {}
                    StepResult::Idle => {
                        // Tasks published after `step` found none would otherwise wait for the
                        // timeout: `sleep` looks at the queues again once registered.
                        sh.idle.fetch_add(1, SeqCst);
                        st.idle_threads.sleep(
                            || st.stop.load(SeqCst) || sh.has_queued(),
                            Duration::from_millis(5),
                        );
                        sh.idle.fetch_sub(1, SeqCst);
                    }
                    StepResult::Blocked | StepResult::WaitingBudget => {
                        thread::sleep(Duration::from_millis(1));
                    }
                }
            }
            w.flush(&sh, &*sk);
            sh.remove_slot(&w.slot);
            if !retired {
                st.active.fetch_sub(1, SeqCst);
            }
        })
        .expect("spawn worker");
    state.handles.lock().push(h);
}

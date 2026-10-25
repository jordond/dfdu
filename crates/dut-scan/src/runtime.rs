//! Real-thread runtime: worker threads and the watchdog thread driving the step functions.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::thread::{self, JoinHandle, Thread};
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::backend::Backend;
use crate::shared::{Clock, Shared, Sink, StepResult, Worker};

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
    stuck_replaced: AtomicUsize,
    idle_threads: Mutex<Vec<Thread>>,
    handles: Mutex<Vec<JoinHandle<()>>>,
}

/// Worker threads plus a watchdog thread.
pub struct Pool {
    state: Arc<PoolState>,
    watchdog: Option<JoinHandle<()>>,
}

/// Most stuck workers replaced at once (`data-pipeline.md` §5.4).
fn stuck_cap(workers: usize) -> usize {
    workers.min(16)
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
            stuck_replaced: AtomicUsize::new(0),
            idle_threads: Mutex::new(Vec::new()),
            handles: Mutex::new(Vec::new()),
        });
        let st = state.clone();
        shared.set_waker(Box::new(move || {
            for t in st.idle_threads.lock().drain(..) {
                t.unpark();
            }
        }));
        let clock = Arc::new(RealClock::new());
        for _ in 0..workers {
            spawn_worker(&state, &shared, &sink, &clock);
        }
        let (st, sh, sk, ck) = (state.clone(), shared.clone(), sink.clone(), clock.clone());
        let watchdog = thread::Builder::new()
            .name("dut-watchdog".into())
            .spawn(move || {
                while !st.stop.load(SeqCst) {
                    let stuck = sh.watchdog_round(&*sk, ck.now());
                    let replaced = st.stuck_replaced.load(SeqCst);
                    if stuck > replaced && replaced < stuck_cap(st.target) {
                        st.stuck_replaced.fetch_add(1, SeqCst);
                        spawn_worker(&st, &sh, &sk, &ck);
                    } else if stuck < replaced {
                        st.stuck_replaced.store(stuck, SeqCst);
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

    /// Stops all threads. Threads stuck in a filesystem call are not waited for.
    pub fn stop(mut self, wait: Duration) {
        self.state.stop.store(true, SeqCst);
        for t in self.state.idle_threads.lock().drain(..) {
            t.unpark();
        }
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
        .name("dut-worker".into())
        .spawn(move || {
            let mut w = Worker::new(&sh, true);
            loop {
                if st.stop.load(SeqCst) {
                    break;
                }
                // A replacement worker made the pool larger than its target: the first worker
                // to find itself without a task leaves.
                if !w.is_running()
                    && st.active.load(SeqCst) > st.target + st.stuck_replaced.load(SeqCst)
                {
                    break;
                }
                match w.step(&sh, &*sk, &*ck) {
                    StepResult::Progress => {}
                    StepResult::Idle => {
                        sh.idle.fetch_add(1, SeqCst);
                        st.idle_threads.lock().push(thread::current());
                        thread::park_timeout(Duration::from_millis(5));
                        sh.idle.fetch_sub(1, SeqCst);
                    }
                    StepResult::Blocked | StepResult::WaitingBudget => {
                        thread::sleep(Duration::from_millis(1));
                    }
                }
            }
            w.flush(&sh, &*sk);
            sh.remove_slot(&w.slot);
            st.active.fetch_sub(1, SeqCst);
        })
        .expect("spawn worker");
    state.handles.lock().push(h);
}

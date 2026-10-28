//! The engine's helper threads: named, and pools of them answering requests.

use std::thread::{self, JoinHandle};

use crossbeam_channel::{Sender, unbounded};

/// Starts a thread named `name` running `f`.
///
/// # Panics
///
/// If the system cannot create the thread, as [`std::thread::spawn`] does.
#[expect(
    clippy::expect_used,
    reason = "as std::thread::spawn: a thread that cannot be created means the system is out of \
              threads or memory, which the engine's callers have no way to recover from"
)]
pub(crate) fn spawn<T: Send + 'static>(
    name: &str,
    f: impl FnOnce() -> T + Send + 'static,
) -> JoinHandle<T> {
    thread::Builder::new()
        .name(name.into())
        .spawn(f)
        .expect("failed to spawn a thread")
}

/// Starts `n` threads (at least one) named `name-0`, `name-1`…, each with its own state from
/// `init`, answering the requests sent on the returned channel with `answer` and sending the
/// answers to `out`. They exit when the returned sender is dropped.
///
/// # Panics
///
/// If the system cannot create a thread ([`spawn`]).
pub(crate) fn spawn_pool<Q, A, S>(
    name: &str,
    n: usize,
    out: &Sender<A>,
    init: impl Fn() -> S + Send + Copy + 'static,
    answer: impl Fn(&mut S, Q) -> A + Send + Copy + 'static,
) -> Sender<Q>
where
    Q: Send + 'static,
    A: Send + 'static,
{
    let (tx, rx) = unbounded::<Q>();
    for i in 0..n.max(1) {
        let (rx, out) = (rx.clone(), out.clone());
        spawn(&format!("{name}-{i}"), move || {
            let mut state = init();
            for req in rx {
                let _ = out.send(answer(&mut state, req));
            }
        });
    }
    tx
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Answers of a pool of `n` threads to the numbers below 100: the number doubled, and the
    /// count of requests its thread had answered.
    fn answers(n: usize) -> Vec<(u32, u32)> {
        let (out, answers) = unbounded();
        let reqs = spawn_pool(
            "dfdu-test-pool",
            n,
            &out,
            || 0u32,
            |seen, x: u32| {
                *seen += 1;
                (x * 2, *seen)
            },
        );
        drop(out);
        for x in 0..100 {
            reqs.send(x).unwrap();
        }
        drop(reqs);
        answers.iter().collect()
    }

    #[test]
    fn a_pool_answers_every_request_and_ends_with_its_sender() {
        let mut got = answers(3);
        got.sort_unstable();
        assert!(got.iter().map(|&(a, _)| a).eq((0..100).map(|x| x * 2)));
    }

    #[test]
    fn each_thread_keeps_its_own_state() {
        let got = answers(1);
        assert!(got.iter().map(|&(_, seen)| seen).eq(1..=100));
    }

    #[test]
    fn spawned_threads_are_named() {
        let name = spawn("dfdu-test-named", || {
            thread::current().name().map(String::from)
        });
        assert_eq!(name.join().unwrap().as_deref(), Some("dfdu-test-named"));
    }
}

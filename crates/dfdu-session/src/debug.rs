//! The debug counters: the pipeline's statistics and timings, the engine's counters outside the
//! model, the byte budget, the running jobs and the stalled directories, copied with every read
//! of the model ([`read`]). `Timing` is not part of the model's fingerprint, so reading them adds
//! no frames of their own.

use dfdu_engine::{Engine, Model, Stats, Timing};
use dfdu_scan::{Backend, Budget};
use dfdu_tree::HostPath;

/// Stalled directories listed by path.
pub const STALLED_SHOWN: usize = 3;

/// Counters kept outside the model, read with it.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Counters {
    /// Scanner messages sent to the applier ([`Engine::sent_messages`]).
    pub sent: u64,
    /// Scanner tasks not finished, and how many of them were abandoned.
    pub outstanding: (u64, u64),
    /// Published tasks not yet taken: `(priority, normal)`.
    pub queued: (usize, usize),
}

impl Counters {
    /// Reads the engine's counters; cheap, and done under the read lock so that `sent` is never
    /// below the messages the model has received.
    pub fn of<B: Backend>(engine: &Engine<B>) -> Self {
        Counters {
            sent: engine.sent_messages(),
            outstanding: engine
                .local_scanner()
                .map(|s| s.outstanding())
                .unwrap_or_default(),
            queued: engine
                .local_scanner()
                .map(|s| s.queued())
                .unwrap_or_default(),
        }
    }
}

/// What [`read`] copied under the read lock.
#[derive(Clone, Debug, Default)]
pub struct Info {
    pub stats: Stats,
    pub timing: Timing,
    pub counters: Counters,
    pub budget: BudgetUse,
    pub running: usize,
    pub stalled: usize,
    /// The first [`STALLED_SHOWN`] stalled directories.
    pub stalled_paths: Vec<HostPath>,
}

/// The byte budget on scanner data in transit, in bytes.
#[derive(Clone, Copy, Debug, Default)]
pub struct BudgetUse {
    /// In transit now.
    pub used: usize,
    /// The most in transit at once so far.
    pub peak: usize,
    pub capacity: usize,
}

impl BudgetUse {
    /// The budget's use now.
    pub fn of(budget: &Budget) -> Self {
        BudgetUse {
            used: budget.used(),
            peak: budget.peak(),
            capacity: budget.capacity(),
        }
    }
}

/// Copies the counters; runs under the read lock. Read whether a front end shows them or not (a
/// copy of two histograms and at most [`STALLED_SHOWN`] paths), so it has numbers as soon as it
/// does.
pub fn read(m: &Model, budget: &Budget, counters: Counters) -> Info {
    let s = &m.status;
    Info {
        stats: s.stats.clone(),
        timing: s.timing.clone(),
        counters,
        budget: BudgetUse::of(budget),
        running: s.running_jobs(),
        stalled: s.stalled.len(),
        stalled_paths: s
            .stalled
            .iter()
            .filter_map(|st| m.tree.resolve_visible(st.node))
            .take(STALLED_SHOWN)
            .map(|ix| m.tree.host_path(ix))
            .collect(),
    }
}

//! The limits of the mounts the scan entered: how many parts may run at once on a mount on a
//! spinning disk or a network mount, and the workers the pool adds for them.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering::SeqCst;

use super::{Shared, Task};
use crate::backend::Backend;
use crate::gate::{Gate, GateStats};

/// At most this many workers are added to the pool for limited mounts, all mounts together.
pub const MAX_MOUNT_WORKERS: usize = 16;

/// A mount's concurrency limit, holding the tasks that wait for a permit.
pub type TaskGate<L> = Gate<Task<L>>;

/// Storage class and limit of each mount the scan entered, by mount key; the limit is `None`
/// for a mount without one.
pub(super) type Gates<L> = HashMap<(u64, u64), (dfdu_sys::Storage, Option<Arc<TaskGate<L>>>)>;

/// A mount's key in [`Shared::gates`]: its id, or its device when there are no mount ids.
fn mount_key(mount: u64, dev: u64) -> (u64, u64) {
    if mount != 0 { (mount, 0) } else { (0, dev) }
}

impl<B: Backend> Shared<B> {
    /// The storage class and the limit of the mount `(mount, dev)`, created when the scan first
    /// enters the mount: `Limits::rotational_parts` on a spinning disk, `Limits::network_parts`
    /// on a network mount, none on other storage. The mount of a job root gets the pool's size
    /// instead; any other limited mount adds its limit to the pool, so its tasks do not take
    /// workers from the rest of the scan. The backend answers from the mount table and sysfs,
    /// without touching the mount.
    pub(super) fn gate_for(
        &self,
        mount: u64,
        dev: u64,
        job_root: bool,
    ) -> (dfdu_sys::Storage, Option<Arc<TaskGate<B::Loc>>>) {
        let key = mount_key(mount, dev);
        let mut gates = self.gates.lock();
        if let Some(g) = gates.get(&key) {
            return g.clone();
        }
        let storage = self.backend.mount_storage(mount, dev);
        let class_limit = match storage {
            dfdu_sys::Storage::Rotational => self.limits.rotational_parts,
            dfdu_sys::Storage::Network => self.limits.network_parts,
            dfdu_sys::Storage::Solid | dfdu_sys::Storage::Unknown => 0,
        };
        let limit = match class_limit {
            0 => 0,
            _ if job_root && self.limits.pool_workers > 0 => self.limits.pool_workers,
            l => l,
        };
        let gate = (limit > 0).then(|| Arc::new(Gate::new(key.0, key.1, limit)));
        if gate.is_some() && !job_root {
            let extra = self.mount_workers.load(SeqCst);
            self.mount_workers
                .store((extra + limit).min(MAX_MOUNT_WORKERS), SeqCst);
        }
        gates.insert(key, (storage, gate.clone()));
        (storage, gate)
    }

    /// Workers the pool adds for limited mounts the scans entered.
    pub fn mount_workers(&self) -> usize {
        self.mount_workers.load(SeqCst)
    }

    /// Every mount limit created so far.
    pub fn gate_stats(&self) -> Vec<GateStats> {
        let gates: Vec<_> = self
            .gates
            .lock()
            .values()
            .filter_map(|g| g.1.clone())
            .collect();
        gates.iter().map(|g| g.stats()).collect()
    }
}

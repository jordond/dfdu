use std::collections::HashMap;
use std::path::PathBuf;

use dfdu_sys::Storage;

use super::*;
use crate::shared::Limits;
use crate::shared::test_util::Local;

const SPINNING: u64 = 1;
const NETWORK: u64 = 2;
const SOLID: u64 = 3;

/// A scan with mounts on a spinning disk, a network mount and solid storage, limited by
/// `limits`.
fn shared(limits: Limits) -> Shared<Local> {
    let storage = HashMap::from([
        (SPINNING, Storage::Rotational),
        (NETWORK, Storage::Network),
        (SOLID, Storage::Solid),
    ]);
    Shared::new(Local::with_storage(storage), limits)
}

/// The number of permits of `gate`.
fn limit(gate: Option<&Arc<TaskGate<PathBuf>>>) -> Option<usize> {
    gate.map(|g| g.stats().limit)
}

#[test]
fn a_mount_is_keyed_by_its_id_or_else_its_device() {
    assert_eq!(mount_key(5, 9), (5, 0));
    assert_eq!(mount_key(0, 9), (0, 9));
}

/// Solid storage and storage the backend does not know have no limit, and add no workers.
#[test]
fn mounts_on_other_storage_have_no_limit() {
    let sh = shared(Limits::default());
    let (storage, gate) = sh.gate_for(SOLID, 1, false);
    assert!(storage == Storage::Solid && gate.is_none());
    let (storage, gate) = sh.gate_for(99, 1, false);
    assert!(storage == Storage::Unknown && gate.is_none());
    assert_eq!(sh.mount_workers(), 0);
    assert!(sh.gate_stats().is_empty());
}

/// A spinning disk gets `rotational_parts`, a network mount `network_parts`, and each adds its
/// limit to the pool the first time the scan enters it.
#[test]
fn a_limited_mount_gets_its_storages_limit_once() {
    let sh = shared(Limits {
        rotational_parts: 2,
        network_parts: 3,
        ..Limits::default()
    });
    let (storage, spinning) = sh.gate_for(SPINNING, 1, false);
    assert_eq!(storage, Storage::Rotational);
    assert_eq!(limit(spinning.as_ref()), Some(2));
    assert_eq!(sh.mount_workers(), 2);
    let (storage, network) = sh.gate_for(NETWORK, 1, false);
    assert_eq!(storage, Storage::Network);
    assert_eq!(limit(network.as_ref()), Some(3));
    assert_eq!(sh.mount_workers(), 5);
    // Entered again, the mount has the limit it got the first time and adds no workers.
    let (_, again) = sh.gate_for(SPINNING, 1, false);
    assert!(Arc::ptr_eq(
        &again.expect("a limit"),
        &spinning.expect("a limit")
    ));
    assert_eq!(sh.mount_workers(), 5);
    let mut stats: Vec<_> = sh.gate_stats().iter().map(|g| (g.mount, g.limit)).collect();
    stats.sort_unstable();
    assert_eq!(stats, [(SPINNING, 2), (NETWORK, 3)]);
}

/// A job root's limited mount gets the pool's size, from the pool's own workers, or its
/// storage's limit when the pool's size is not set.
#[test]
fn a_job_roots_mount_gets_the_pools_size() {
    let sized = shared(Limits {
        pool_workers: 8,
        ..Limits::default()
    });
    assert_eq!(limit(sized.gate_for(SPINNING, 1, true).1.as_ref()), Some(8));
    assert_eq!(sized.mount_workers(), 0);
    // A job root on solid storage still has no limit.
    assert!(sized.gate_for(SOLID, 1, true).1.is_none());

    let unsized_pool = shared(Limits::default());
    let rotational = Limits::default().rotational_parts;
    let gate = unsized_pool.gate_for(SPINNING, 1, true).1;
    assert_eq!(limit(gate.as_ref()), Some(rotational));
    assert_eq!(unsized_pool.mount_workers(), 0);
}

/// Without mount ids each device is a mount of its own.
#[test]
fn mounts_without_ids_are_told_apart_by_device() {
    let sh = shared(Limits::default());
    let (_, a) = sh.gate_for(0, SPINNING, false);
    let (_, b) = sh.gate_for(0, NETWORK, false);
    let (_, a_again) = sh.gate_for(0, SPINNING, false);
    let (a, b, a_again) = (a.expect("a"), b.expect("b"), a_again.expect("a again"));
    assert!(!Arc::ptr_eq(&a, &b) && Arc::ptr_eq(&a, &a_again));
    assert_eq!((a.stats().mount, a.stats().dev), (0, SPINNING));
}

/// However many limited mounts the scan enters, the pool grows by at most
/// [`MAX_MOUNT_WORKERS`].
#[test]
fn the_workers_added_for_mounts_are_capped() {
    let sh = shared(Limits {
        rotational_parts: 10,
        network_parts: 10,
        ..Limits::default()
    });
    sh.gate_for(SPINNING, 1, false);
    sh.gate_for(NETWORK, 1, false);
    assert_eq!(sh.mount_workers(), MAX_MOUNT_WORKERS);
}

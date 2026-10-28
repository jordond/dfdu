//! The native snapshot decoder: any input is read or refused, never a panic or a hang.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let limits = dfdu_store::ncdu::Limits {
        max_entries: 200_000,
        ..Default::default()
    };
    if let Ok(l) = dfdu_store::snapshot::read(data, &limits) {
        // What was read is a tree that satisfies the tree's own checks.
        let _ = l.tree.total(l.root);
    }
});

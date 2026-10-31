//! `dfdu -f`'s reader: ncdu JSON, plain or zstd, or a snapshot, told apart by its first bytes.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let limits = dfdu_store::Limits {
        max_entries: 200_000,
        ..Default::default()
    };
    if let Ok(o) = dfdu_store::open(data, &limits) {
        let _ = o.tree.total(o.root);
    }
});

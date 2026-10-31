//! `dfdu -f`'s reader: ncdu JSON, plain or zstd, or a snapshot, told apart by its first bytes.
#![no_main]

mod common;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let limits = common::limits();
    if let Ok(o) = dfdu_store::open(data, &limits) {
        let _ = o.tree.total(o.root);
    }
});

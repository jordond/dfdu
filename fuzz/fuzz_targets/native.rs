//! What native size commands print (`docker system df --format json`).
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = dfdu_engine::parse_reclaimable(&String::from_utf8_lossy(data));
});

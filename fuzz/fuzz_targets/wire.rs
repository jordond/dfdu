//! The remote scan's wire format as the local side reads it: a stream of frames from an agent,
//! each payload decoded as an agent message, and as an agent reads the local side's.
#![no_main]

use std::sync::LazyLock;

use dfdu_rules::{Env, Platform, RuleSet};
use dfdu_engine::wire::{Down, Up, read_frame};
use libfuzzer_sys::fuzz_target;

static RULES: LazyLock<RuleSet> = LazyLock::new(|| {
    dfdu_rules::builtin(&Env::with(Platform::Linux, &[("HOME", "/home/u")])).0
});

fuzz_target!(|data: &[u8]| {
    let rules = Some(&*RULES);
    let _ = Up::decode(data, rules);
    let _ = Up::decode(data, None);
    let _ = Down::decode(data, rules);
    let _ = Down::decode(data, None);
    let mut r = data;
    let mut buf = Vec::new();
    while let Ok(true) = read_frame(&mut r, &mut buf) {
        if let Ok(Up::Batch(msgs)) = Up::decode(&buf, rules) {
            for m in msgs {
                if let dfdu_scan::ScanMsg::Chunk(c) = m {
                    for f in &c.files {
                        let _ = c.name(f.name);
                    }
                }
            }
        }
    }
});

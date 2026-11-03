//! The remote scan's wire format as the local side reads it: a stream of frames from an agent,
//! each payload decoded as an agent message, and as an agent reads the local side's.
#![no_main]

use std::sync::LazyLock;

use dfdu_rules::{Env, Platform, RuleSet};
use dfdu_engine::wire::{self, ToAgent, FromAgent, read_frame};
use libfuzzer_sys::fuzz_target;

static RULES: LazyLock<RuleSet> = LazyLock::new(|| {
    dfdu_rules::builtin(&Env::with(Platform::Linux, &[("HOME", "/home/u")])).0
});

fuzz_target!(|data: &[u8]| {
    let rules = Some(&*RULES);
    let _ = FromAgent::decode(data, rules, dfdu_tree::Host::LOCAL);
    let _ = FromAgent::decode(data, None, dfdu_tree::Host::LOCAL);
    // Listings and mount lists are bounded, and a name never holds a separator of its host.
    for host in [dfdu_tree::Host::Unix, dfdu_tree::Host::Windows] {
        match FromAgent::decode(data, None, host) {
            Ok(FromAgent::Listed { result: Ok(entries), .. }) => {
                assert!(entries.len() <= wire::listing::MAX_ENTRIES);
                for e in &entries {
                    assert!(e.name.len() <= wire::listing::MAX_NAME);
                    assert!(!e.name.is_empty() && !e.name.iter().any(|&b| host.is_sep(b)));
                }
            }
            Ok(FromAgent::Mounts { result: Ok(mounts), .. }) => {
                assert!(mounts.len() <= wire::listing::MAX_MOUNTS);
            }
            _ => {}
        }
    }
    let _ = ToAgent::decode(data, rules);
    let _ = ToAgent::decode(data, None);
    let mut r = data;
    let mut buf = Vec::new();
    while let Ok(true) = read_frame(&mut r, &mut buf) {
        if let Ok(FromAgent::Batch(msgs)) = FromAgent::decode(&buf, rules, dfdu_tree::Host::LOCAL) {
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

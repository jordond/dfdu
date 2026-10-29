//! What a run is configured with, and what it reports.

use std::sync::Arc;

use dfdu_rules::RuleSet;
use dfdu_scan::Limits;

use crate::SpecConfig;

#[derive(Clone)]
pub struct SimConfig {
    pub workers: usize,
    pub budget: usize,
    pub limits: Limits,
    pub auto_abandon_ns: u64,
    pub rescans: u32,
    /// Delete commands issued at random points; results are chosen by the simulated deleter.
    pub deletes: u32,
    /// Cancel commands for random running jobs.
    pub cancels: u32,
    pub max_ticks: u64,
    pub spec: SpecConfig,
    /// Scan with `cross_mounts`; `None` picks it per seed.
    pub cross_mounts: Option<bool>,
    /// Scan with `network`; `None` picks it per seed.
    pub network: Option<bool>,
    /// `Limits::rotational_parts` and `Limits::network_parts`; `None` picks them per seed
    /// (1-2 and 1-3).
    pub mount_limits: Option<(usize, usize)>,
    /// Classify against these rules, and check every class against a reference at the end.
    pub rules: Option<Arc<RuleSet>>,
    /// Runs the classifier pool: its requests are answered at random points, with answers
    /// that depend on the node's path ([`sim_check`](crate::sim_check)), and checked against them at the end.
    pub checks: bool,
}

impl Default for SimConfig {
    fn default() -> Self {
        SimConfig {
            workers: 3,
            budget: 16 * 1024,
            limits: Limits {
                block_entries: 3,
                block_name_bytes: 1 << 20,
                outbox_bytes: 4 * 1024,
                outbox_msgs: 5,
                flush_age_ns: 2_000_000,
                stall_ns: 10_000_000,
                chunk_credit: 2,
                ..Limits::default()
            },
            auto_abandon_ns: 400_000_000,
            rescans: 3,
            deletes: 3,
            cancels: 1,
            max_ticks: 2_000_000,
            spec: SpecConfig::default(),
            cross_mounts: None,
            network: None,
            mount_limits: None,
            rules: None,
            checks: false,
        }
    }
}

/// The built-in rules for a Linux home at `/home/u`, so that nothing below [`ROOT_NAME`](crate::ROOT_NAME) is a
/// protected or a known path, and only rules matched by name apply; and a rule for empty
/// directories anywhere below it, decided when their listings end.
///
/// # Panics
///
/// If the built-in rules or the empty-directory rule do not load, a bug in them.
#[must_use]
pub fn sim_rules() -> Arc<RuleSet> {
    const EMPTY: &str = r#"schema_version = 1
[[rule]]
id = "sim.empty"
category = "content.empty"
name = "Empty"
safety = "review"
priority = -10
[rule.match]
kind = "dir"
paths = ["/sim/**"]
empty = true
"#;
    let env = dfdu_rules::Env::with(dfdu_rules::Platform::Linux, &[("HOME", "/home/u")]);
    let (set, errors) = dfdu_rules::load_from(
        &env,
        &[
            (
                dfdu_rules::Source::Builtin,
                dfdu_rules::load::BUILTIN.into(),
            ),
            (dfdu_rules::Source::File("sim.toml".into()), EMPTY.into()),
        ],
    );
    assert!(errors.is_empty(), "{errors:?}");
    Arc::new(set)
}

#[derive(Debug, Default)]
pub struct SimReport {
    pub ticks: u64,
    /// Nodes the reference classifier gave a class to, all checked.
    pub classified: u64,
    /// Classifier pool answers applied, and classes in the final tree flagged git-tracked or
    /// in use by them.
    pub pool_answers: u64,
    pub tracked: u64,
    pub in_use: u64,
    pub dirs: usize,
    pub rescans: u32,
    pub deletes: u32,
    pub deleted_items: u64,
    pub cancels: u32,
    pub recoveries: u32,
    pub abandoned: u64,
    pub max_workers: usize,
    pub max_budget_used: u64,
    pub stalled_msgs: u64,
    pub late: u64,
    pub discarded_cut: u64,
    /// Subvolume records, read-only ones among them, and duplicate mounts in the final tree.
    pub subvols: u64,
    pub ro_subvols: u64,
    pub dup_mounts: u64,
    /// Runs with `cross_mounts`.
    pub cross_runs: u64,
    /// Network and kernel mounts left unscanned in the final tree.
    pub network_skipped: u64,
    pub kernel_skipped: u64,
    /// Measurements answered, and bytes counted once instead of per file in the final tree.
    pub measured: u64,
    pub shared_bytes: u64,
    /// Blocks stat'ed by chunk tasks rather than by their directory's listing task.
    pub chunk_tasks: u64,
    /// Mount limits created, tasks that waited for a permit, permits taken back from stuck
    /// parts, and checks that found a mount running at its limit.
    pub gates: u64,
    pub deferred: u64,
    pub revoked: u64,
    pub at_limit: u64,
}

//! The end of a run: stepping until everything has settled, rescanning what cancels left
//! incomplete, and the final checks against the spec.

use std::sync::atomic::Ordering::SeqCst;

use dfdu_engine::Command;
use dfdu_tree::NodeFlags;

use crate::check::{check_classes, check_pool_flags, compare, topmost_incomplete};
use crate::{Sim, SimReport};

impl Sim {
    /// Nothing is running, queued or in flight anywhere.
    #[must_use]
    pub fn settled(&self) -> bool {
        let (out, ab) = self.shared.outstanding();
        out == ab
            && self.model.status.running() == 0
            && !self.applier.has_work()
            && !self.model.status.settling
            && self.cur.is_none()
            && self.sink.q.lock().is_empty()
            && self.model.status.commands_applied > 0
            && self.plans.is_empty()
            && self.del_q.is_empty()
            && self.model.status.deleting.is_none()
            && self.meas_q.is_empty()
            && self.model.status.measuring.is_none()
            && self.check_q.is_empty()
    }

    /// Steps in random order until everything has settled and no directory is left
    /// incomplete, then checks the final tree against the spec.
    ///
    /// # Errors
    ///
    /// When a check of the run fails; the message names the seed and what went wrong.
    pub fn run_to_end(mut self) -> Result<SimReport, String> {
        self.settle()?;
        self.finish()
    }

    /// Steps in random order until everything has settled and no directory is left
    /// incomplete.
    pub(crate) fn settle(&mut self) -> Result<(), String> {
        loop {
            if self.report.ticks >= self.cfg.max_ticks {
                let (out, ab) = self.shared.outstanding();
                eprintln!("{}", self.shared.debug_parts());
                return Err(self.fail(format!(
                    "did not finish: running jobs {}, outstanding {out}, abandoned {ab}, queued batches {}",
                    self.model.status.running(),
                    self.sink.q.lock().len()
                )));
            }
            self.random_step()?;
            if self.settled() {
                // A cancelled job leaves directories incomplete (Cancelled, or Superseded when
                // a cancelled rescan had cut them). Rescan the topmost one, as a user would,
                // until none remain; then the tree must match the filesystem again.
                match topmost_incomplete(&self.model.tree) {
                    Some(ix) if self.recoveries < 100 => {
                        self.recoveries += 1;
                        self.report.recoveries += 1;
                        let r = self.model.tree.node_ref(ix);
                        self.applier
                            .apply_command(&mut self.model, Command::Rescan { node: r });
                    }
                    Some(_) => {
                        return Err(self.fail("incomplete directories remain after 100 rescans"));
                    }
                    None => return Ok(()),
                }
            }
        }
    }

    pub(crate) fn finish(mut self) -> Result<SimReport, String> {
        let (spec, model) = (&self.spec, &self.model);
        let report = &mut self.report;
        report.chunk_tasks = self.shared.chunk_tasks.load(SeqCst);
        for g in self.shared.gate_stats() {
            report.gates += 1;
            report.deferred += g.deferred;
            report.revoked += g.revoked;
            // Parts still stuck forever lost their permits; nothing else is left running.
            if g.running != 0 || g.pending != 0 {
                return Err(format!(
                    "seed {}: mount limit left with work at the end: {g:?}",
                    self.seed
                ));
            }
        }
        report.subvols = model.subvols.len() as u64;
        report.ro_subvols = model.subvols.values().filter(|s| s.read_only).count() as u64;
        report.cross_runs = u64::from(self.cross_mounts);
        if let Some(root) = model.tree.roots().next() {
            let t = model.tree.total(root);
            report.shared_bytes = t.referenced.saturating_sub(t.alloc);
            let mut stack = vec![root];
            while let Some(ix) = stack.pop() {
                let f = model.tree.flags(ix);
                report.dup_mounts += u64::from(f.has(NodeFlags::DUP_MOUNT));
                report.network_skipped += u64::from(f.has(NodeFlags::NETWORK));
                report.kernel_skipped += u64::from(f.has(NodeFlags::KERNEL));
                stack.extend(model.tree.children(ix));
            }
        }
        let st = &model.status.stats;
        report.max_budget_used = self.sink.max_used.load(SeqCst);
        report.stalled_msgs = st.stalled;
        report.late = st.late;
        report.discarded_cut = st.discarded_cut;
        if st.corrupt + st.lost + st.duplicate > 0 {
            return Err(self.fail(format!(
                "corrupt {} lost {} duplicate {}: {:?}",
                st.corrupt,
                st.lost,
                st.duplicate,
                &model.status.anomalies[..model.status.anomalies.len().min(5)]
            )));
        }
        let used = report.max_budget_used;
        if used > self.cfg.budget as u64 {
            return Err(self.fail(format!("budget exceeded: {used}")));
        }
        let seed = self.seed;
        self.applier
            .check(model)
            .map_err(|e| format!("seed {seed}: final invariant: {e}"))?;
        self.applier
            .check_settled(model)
            .map_err(|e| format!("seed {seed}: settled: {e}"))?;
        if let Some(rules) = &self.cfg.rules {
            report.classified = check_classes(spec, model, rules)
                .map_err(|e| format!("seed {seed}: classes: {e}"))?;
            if self.cfg.checks {
                (report.tracked, report.in_use) = check_pool_flags(model, rules)
                    .map_err(|e| format!("seed {seed}: classifier pool: {e}"))?;
            }
        }
        if let Ok(v) = std::env::var("DFDU_SIM_INODE") {
            let mut it = v.split(',').map(str::parse::<u64>);
            let key = match (it.next(), it.next()) {
                (Some(Ok(dev)), Some(Ok(ino))) => (dev, ino),
                _ => return Err(format!("DFDU_SIM_INODE={v}: expected dev,ino")),
            };
            eprintln!("{}", self.applier.describe_extents(model, key));
            for n in spec
                .nodes
                .iter()
                .filter(|n| (n.meta.dev, n.meta.ino) == key)
            {
                eprintln!("  spec pieces {:?}", n.pieces);
            }
        }
        let (dirs, abandoned) = compare(
            spec,
            model,
            self.cfg.limits.block_entries,
            self.cross_mounts,
            self.network,
        )
        .map_err(|e| {
            let n = &model.status.notices;
            format!(
                "seed {seed}: {e}; last notices: {:?}",
                &n[n.len().saturating_sub(6)..]
            )
        })?;
        self.report.dirs = dirs;
        self.report.abandoned = abandoned;
        Ok(self.report)
    }
}

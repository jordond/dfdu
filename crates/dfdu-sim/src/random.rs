//! The random driver: one step at a time, chosen by the seed.

use dfdu_clean::DeleterMsg;
use dfdu_engine::{Command, MeasureReq, MeasureResult, Mode};
use dfdu_scan::Clock;

use crate::check::random_dir;
use crate::deleter::{deleter_step, pick_deletable};
use crate::{Sim, sim_check};

impl Sim {
    /// One step chosen at random: a worker step, answers to the applier's requests, applied
    /// messages and deleter results, a watchdog round, a tick, a deleter step or a command.
    ///
    /// # Errors
    ///
    /// When a check of the run fails; the message names the seed and what went wrong.
    pub fn random_step(&mut self) -> Result<(), String> {
        self.report.ticks += 1;
        self.check_q.extend(self.applier.take_check_requests());
        // One request per file, so answers can arrive in any order.
        for r in self.applier.take_measure_requests() {
            for f in r.files {
                self.meas_q.push(MeasureReq {
                    round: r.round,
                    dir: r.dir.clone(),
                    files: vec![f],
                });
            }
        }
        let r = self.rng.below(100);
        match r {
            0..55 => {
                let i = self.rng.index(self.workers.len());
                self.step_worker(i);
                self.check_mount_limits()?;
            }
            55..60 if !self.check_q.is_empty() && self.rng.chance(400) => {
                for _ in 0..=self.rng.below(4) {
                    if self.check_q.is_empty() {
                        break;
                    }
                    let i = self.rng.index(self.check_q.len());
                    let msg = sim_check(&self.check_q.swap_remove(i));
                    self.report.pool_answers += 1;
                    self.applier.apply_check(&mut self.model, msg);
                    self.check(" after a classifier answer")?;
                }
            }
            55..60 => {
                // Answers in any order, interleaved with everything else.
                for _ in 0..=self.rng.below(8) {
                    if self.meas_q.is_empty() {
                        break;
                    }
                    let i = self.rng.index(self.meas_q.len());
                    let req = self.meas_q.swap_remove(i);
                    let msg = self.shared.backend.measure(&req);
                    self.report.measured +=
                        u64::from(matches!(msg.result, MeasureResult::Measured(_)));
                    let now = self.clock.now();
                    self.applier.apply_measure(&mut self.model, msg, now);
                    self.check(" after measurement")?;
                }
            }
            60..80 => {
                for _ in 0..=self.rng.below(20) {
                    if let Some(d) = self.del_q.pop_front() {
                        if let (Some(tr), DeleterMsg::Item { node, result, .. }) = (trace(), &d) {
                            match self.model.tree.resolve(*node) {
                                Some(ix) => {
                                    let p = String::from_utf8_lossy(&self.model.tree.path(ix))
                                        .into_owned();
                                    if tr.starts_with(&p) || p.starts_with(&tr) {
                                        eprintln!("[{}] result {p}: {result:?}", self.report.ticks);
                                    }
                                }
                                None => eprintln!(
                                    "[{}] result for unresolvable node: {result:?}",
                                    self.report.ticks
                                ),
                            }
                        }
                        self.applier.apply_delete(&mut self.model, d);
                        self.applier.refresh_status(&mut self.model);
                        self.check("")?;
                    } else if self.applier.has_work() {
                        self.applier.work_step(&mut self.model);
                        self.applier.refresh_status(&mut self.model);
                        self.check("")?;
                    } else if self.apply_next()?.is_none() {
                        break;
                    }
                }
                self.plans
                    .extend(self.applier.take_plans().into_iter().map(|p| (p, 0)));
            }
            80..90 => {
                self.watchdog();
                self.check_mount_limits()?;
            }
            90..93 => self.tick(),
            93..96 => {
                if let Some(msg) = deleter_step(
                    &self.spec,
                    &self.shared,
                    &mut self.plans,
                    &mut self.rng,
                    &mut self.report,
                ) {
                    self.del_q.push_back(msg);
                }
            }
            _ => self.random_command()?,
        }
        self.check_q.extend(self.applier.take_check_requests());
        Ok(())
    }

    /// Time passes, and maybe a cancel, a delete or a rescan is issued.
    fn random_command(&mut self) -> Result<(), String> {
        self.clock.advance(self.rng.below(3) * 1_000_000);
        if self.cancels_left > 0 && self.rng.chance(8) {
            let running: Vec<_> = self
                .model
                .status
                .jobs
                .iter()
                .filter(|j| j.phase == dfdu_engine::JobPhase::Running)
                .map(|j| j.id)
                .collect();
            if !running.is_empty() {
                let job = running[self.rng.index(running.len())];
                self.cancels_left -= 1;
                self.report.cancels += 1;
                self.command(Command::Cancel { job })?;
            }
        }
        if self.deletes_left > 0 && self.rng.chance(15) {
            let items = pick_deletable(&self.model.tree, &self.spec, &self.shared, &mut self.rng);
            if !items.is_empty() {
                self.deletes_left -= 1;
                self.report.deletes += 1;
                if let Some(tr) = trace() {
                    for r in &items {
                        if let Some(ix) = self.model.tree.resolve(*r) {
                            let p = String::from_utf8_lossy(&self.model.tree.path(ix)).into_owned();
                            if tr.starts_with(&p) || p.starts_with(&tr) {
                                eprintln!("[{}] delete submitted {p}", self.report.ticks);
                            }
                        }
                    }
                }
                self.command(Command::Delete {
                    items,
                    mode: Mode::Permanent,
                })?;
            }
        }
        if self.rescans_left > 0
            && self.rng.chance(20)
            && let Some(node) = random_dir(&self.model.tree, &mut self.rng)
        {
            self.rescans_left -= 1;
            self.report.rescans += 1;
            let r = self.model.tree.node_ref(node);
            if let Some(tr) = trace() {
                let p = String::from_utf8_lossy(&self.model.tree.path(node)).into_owned();
                if tr.starts_with(&p) {
                    eprintln!("[{}] rescan requested {p}", self.report.ticks);
                }
            }
            self.command(Command::Rescan { node: r })?;
        }
        Ok(())
    }
}

fn trace() -> Option<String> {
    std::env::var("DFDU_SIM_TRACE").ok()
}

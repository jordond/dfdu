//! Guard checks and native size commands, answered by the classifier pool.

use dfdu_scan::Backend;
use dfdu_tree::{Class, Ix, RuleIx, Safety};

use super::{Applier, Model};
use crate::classifier::{CheckMsg, CheckReq};

impl<B: Backend> Applier<B> {
    /// Requests for the classifier pool.
    pub fn take_check_requests(&mut self) -> Vec<CheckReq> {
        std::mem::take(&mut self.outgoing_checks)
    }

    /// Queues the guard checks of every visible class not checked or queued yet. Classes are
    /// few (one per match, nothing below it), so this walks all of them.
    pub(super) fn queue_checks(&mut self, m: &mut Model) {
        let Some(rules) = m.rules.clone().filter(|_| self.settings.checks) else {
            return;
        };
        let todo: Vec<(Ix, Class)> = m
            .tree
            .classes()
            .filter(|(ix, c)| !c.has(Class::QUEUED | Class::CHECKED) && m.tree.is_visible(*ix))
            .collect();
        for (ix, mut c) in todo {
            let g = &rules.rule(c.rule).guard;
            if !g.not_vcs_tracked && g.not_running.is_empty() {
                continue;
            }
            self.outgoing_checks.push(CheckReq::Guards {
                node: m.tree.node_ref(ix),
                rule: c.rule,
                path: m.tree.host_path(ix),
                vcs: g.not_vcs_tracked,
                processes: g.not_running.clone(),
            });
            c.flags |= Class::QUEUED;
            m.tree.set_class(ix, Some(c));
        }
    }

    pub(super) fn native_sizes(&mut self, m: &Model) {
        let Some(rules) = m.rules.as_ref().filter(|_| self.settings.checks) else {
            return;
        };
        let mut matched: Vec<RuleIx> = m.tree.classes().map(|(_, c)| c.rule).collect();
        matched.sort_unstable();
        matched.dedup();
        for rule in matched {
            if let Some(n) = rules.rule(rule).action.native()
                && !n.size_command.is_empty()
            {
                self.outgoing_checks.push(CheckReq::NativeSize {
                    rule,
                    check: n.check.clone(),
                    command: n.size_command.clone(),
                });
            }
        }
    }

    /// Applies an answer of the classifier pool. One for a node freed since, or whose class
    /// changed (a rescan), is dropped.
    pub fn apply_check(&mut self, m: &mut Model, msg: CheckMsg) {
        match msg {
            CheckMsg::Guards {
                node,
                rule,
                tracked,
                running,
            } => {
                let Some(ix) = m.tree.resolve(node) else {
                    return;
                };
                let Some(mut c) = m.tree.class(ix).filter(|c| c.rule == rule) else {
                    return;
                };
                c.flags = (c.flags & !Class::QUEUED) | Class::CHECKED;
                match tracked {
                    Some(true) => {
                        c.flags |= Class::VCS_TRACKED;
                        c.safety = Safety::Protected;
                    }
                    Some(false) => {}
                    None => c.flags |= Class::VCS_UNKNOWN,
                }
                if running.is_some() {
                    c.flags |= Class::IN_USE;
                }
                m.tree.set_class(ix, Some(c));
                m.tree.touch(ix);
            }
            CheckMsg::NativeSize { rule, result } => {
                m.native_sizes.insert(rule, result);
                m.tree.bump_version();
            }
        }
    }
}

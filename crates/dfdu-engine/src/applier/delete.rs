//! Delete plans: checking and queueing them, running one at a time, and applying the
//! deleter's results.

use std::collections::HashSet;

use dfdu_clean::{DeleteResult, DeleterMsg, DeleterReq, Identity, Mode, Plan, PlanItem};
use dfdu_scan::Backend;
use dfdu_tree::{DirState, Ix, Kind, NodeFlags, NodeRef, Tree};

use super::work::Work;
use super::{
    Applier, DeleteProgress, DeleteSummary, JobKind, Model, NATIVE_LINES, PROBLEMS_KEPT, Status,
};

pub(super) struct ActivePlan {
    id: u64,
    mode: Mode,
    total: usize,
    remaining: HashSet<NodeRef>,
    summary: DeleteSummary,
}

impl<B: Backend> Applier<B> {
    /// Plans for the deleter thread, taken by the runtime after each slice.
    pub fn take_plans(&mut self) -> Vec<Plan> {
        std::mem::take(&mut self.outgoing_plans)
    }

    /// Undo and commit requests for the deleter thread, sent after the plans.
    pub fn take_deleter_controls(&mut self) -> Vec<DeleterReq> {
        std::mem::take(&mut self.outgoing_controls)
    }

    fn plan_nodes(&self) -> impl Iterator<Item = NodeRef> + '_ {
        let active = self
            .active_plan
            .iter()
            .flat_map(|p| p.remaining.iter().copied());
        let queued = self
            .queued_plans
            .iter()
            .flat_map(|p| p.items.iter().map(|i| i.node));
        active.chain(queued)
    }

    /// Whether `ix`, an ancestor or a descendant is in the running or a queued plan.
    pub(super) fn plan_overlaps(&self, tree: &Tree, ix: Ix) -> bool {
        self.plan_nodes()
            .filter_map(|r| tree.resolve(r))
            .any(|p| p == ix || Self::is_ancestor(tree, p, ix) || Self::is_ancestor(tree, ix, p))
    }

    /// Why the tree says `ix` cannot be deleted; the deleter checks the filesystem again
    /// before touching it.
    fn delete_refusal(tree: &Tree, ix: Ix, privileged: bool) -> Option<String> {
        let flags = tree.flags(ix);
        let t = tree.total(ix);
        if flags.has(NodeFlags::MOUNT_ROOT) {
            return Some("is a mount point; unmount it first".into());
        }
        if tree.is_protected(ix) {
            return Some("is protected (version control, secrets or system data)".into());
        }
        if t.mounts > 0 {
            return Some(format!(
                "contains {} mount point{}; unmount {} first",
                t.mounts,
                if t.mounts == 1 { "" } else { "s" },
                if t.mounts == 1 { "it" } else { "them" }
            ));
        }
        let mut a = ix;
        while let Some(p) = tree.parent(a) {
            if tree.flags(p).has(NodeFlags::RO_SUBVOL) {
                return Some(format!(
                    "is in the read-only snapshot {}",
                    String::from_utf8_lossy(tree.name(p))
                ));
            }
            a = p;
        }
        if t.ro_subvols > 0 && !privileged {
            return Some(if flags.has(NodeFlags::RO_SUBVOL) {
                "is a read-only snapshot: deleting it needs root".into()
            } else {
                "contains a read-only snapshot: deleting it needs root".into()
            });
        }
        None
    }

    /// Queues a plan for `items`. Read-only mode and imported trees are refused before this,
    /// by [`Applier::refusal`].
    pub(super) fn submit_plan(&mut self, m: &mut Model, items: Vec<NodeRef>, mode: Mode) {
        let mut out = Vec::new();
        let mut seen: HashSet<Ix> = HashSet::new();
        for r in items {
            let Some(ix) = m.tree.resolve_visible(r) else {
                continue;
            };
            if m.tree.flags(ix).has(NodeFlags::DELETING) || !seen.insert(ix) {
                continue;
            }
            // Overlap with rescans: a running rescan of the item, an ancestor or a
            // descendant blocks it. Cancelling a descendant's rescan instead would leave that
            // directory cut, with partial totals, if this delete then did not happen.
            let blocked = self
                .jobs
                .values()
                .filter(|j| j.kind == JobKind::Rescan && j.swap_pending())
                .filter_map(|j| m.tree.resolve_visible(j.target))
                .any(|t| {
                    t == ix
                        || Self::is_ancestor(&m.tree, t, ix)
                        || Self::is_ancestor(&m.tree, ix, t)
                });
            if blocked {
                m.status.notice(format!(
                    "delete: {} or something in it is being rescanned; delete after it completes",
                    String::from_utf8_lossy(m.tree.name(ix))
                ));
                continue;
            }
            if super::unrefreshed(&m.tree, ix) {
                m.status.notice(format!(
                    "delete: {} is not refreshed yet: loaded from a file, and the resume has not reached it",
                    String::from_utf8_lossy(m.tree.name(ix))
                ));
                continue;
            }
            if let Some(why) = Self::delete_refusal(&m.tree, ix, self.privileged) {
                m.status.notice(format!(
                    "delete: {} {why}",
                    String::from_utf8_lossy(m.tree.name(ix))
                ));
                continue;
            }
            let inside = std::sync::Arc::new(listings(&m.tree, ix));
            self.cut(m, ix, DirState::Deleting);
            m.tree.set_deleting(ix, true);
            let (dev, ino) = m.tree.dev_ino(ix);
            out.push(PlanItem {
                node: m.tree.node_ref(ix),
                path: m.tree.host_path(ix),
                identity: Identity {
                    dev,
                    ino,
                    kind: m.tree.kind(ix),
                    apparent: m.tree.raw_size(ix).0,
                    mtime: m.tree.mtime(ix),
                },
                bytes: m.tree.total(ix).alloc,
                inside,
            });
        }
        if out.is_empty() {
            return;
        }
        let root = out
            .first()
            .and_then(|i| m.tree.resolve(i.node))
            .map(|mut ix| {
                while let Some(p) = m.tree.parent(ix) {
                    ix = p;
                }
                m.tree.host_path(ix)
            })
            .unwrap_or_else(|| dfdu_tree::HostPath::empty(m.tree.host()));
        let id = self.next_plan;
        self.next_plan += 1;
        self.queued_plans.push_back(Plan {
            id,
            mode,
            scan_root: root,
            items: out,
            protected: m.rules.as_ref().map(|r| r.protected().clone()),
        });
        self.start_next_plan(m);
    }

    fn start_next_plan(&mut self, m: &mut Model) {
        if self.active_plan.is_some() {
            if let Some(d) = m.status.deleting.as_mut() {
                d.queued_plans = self.queued_plans.len();
            }
            return;
        }
        while let Some(mut plan) = self.queued_plans.pop_front() {
            // Re-check items queued behind another plan: they may have gone meanwhile.
            plan.items
                .retain(|i| m.tree.resolve_visible(i.node).is_some());
            if plan.items.is_empty() {
                continue;
            }
            let remaining: HashSet<NodeRef> = plan.items.iter().map(|i| i.node).collect();
            m.status.deleting = Some(DeleteProgress {
                plan: plan.id,
                total: remaining.len(),
                done: 0,
                queued_plans: self.queued_plans.len(),
            });
            self.active_plan = Some(ActivePlan {
                id: plan.id,
                mode: plan.mode,
                total: remaining.len(),
                remaining,
                summary: DeleteSummary {
                    plan: plan.id,
                    mode: Some(plan.mode),
                    ..Default::default()
                },
            });
            self.outgoing_plans.push(plan);
            return;
        }
        m.status.deleting = None;
    }

    pub(super) fn retry_deferred_rescans(&mut self, m: &mut Model) {
        for r in std::mem::take(&mut self.deferred_rescans) {
            match m.tree.resolve_visible(r) {
                None => {}
                Some(ix) if self.plan_overlaps(&m.tree, ix) => self.deferred_rescans.push(r),
                Some(_) => self.rescan(m, r, None, false),
            }
        }
    }

    /// Applies a result from the deleter.
    pub fn apply_delete(&mut self, m: &mut Model, msg: DeleterMsg) {
        match msg {
            DeleterMsg::Item {
                plan,
                node,
                bytes,
                result,
            } => self.item_result(m, plan, node, bytes, &result),
            DeleterMsg::Done { plan, btrfs } => {
                if btrfs {
                    m.status.notice(
                        "btrfs frees the space when the transaction commits; space of deleted \
                         snapshots is freed in the background, so free space can lag"
                            .into(),
                    );
                }
                if let Some(a) = self.active_plan.take_if(|a| a.id == plan) {
                    for r in a.remaining {
                        if let Some(ix) = m.tree.resolve(r) {
                            m.tree.set_deleting(ix, false);
                        }
                    }
                    m.status.last_delete = Some(DeleteSummary { btrfs, ..a.summary });
                }
                self.retry_deferred_rescans(m);
                self.start_next_plan(m);
            }
            DeleterMsg::Committed {
                plan,
                bytes,
                result,
            } => {
                m.status.staged.remove(bytes);
                self.forget_undoable(plan, bytes);
                if let Err(e) = result {
                    m.status.notice(format!("delete staged: {e}"));
                }
            }
            DeleterMsg::Restored {
                plan: _,
                original,
                staged,
                bytes,
                result,
            } => {
                if staged {
                    m.status.staged.remove(bytes);
                }
                match result {
                    Ok(to) => {
                        if to != original {
                            m.status.notice(format!(
                                "undo: {} exists again; restored as {}",
                                original.display(),
                                to.display()
                            ));
                        }
                        self.rescan_paths.push(to);
                    }
                    Err(e) => m.status.notice(format!("undo {}: {e}", original.display())),
                }
            }
            DeleterMsg::NativeOutput { id, line } => {
                if let Some(n) = m.status.native.as_mut().filter(|n| n.id == id) {
                    if n.lines.len() == NATIVE_LINES {
                        n.lines.pop_front();
                    }
                    n.lines.push_back(line);
                }
            }
            DeleterMsg::NativeDone { id, result } => {
                if let Some(n) = m.status.native.as_mut().filter(|n| n.id == id) {
                    let rescan = !n.dry_run && result.is_ok();
                    n.done = Some(result);
                    if rescan {
                        self.rescan_paths(m);
                    }
                }
                self.rescan_paths.clear();
            }
            DeleterMsg::UndoDone { plan } => {
                if m.status.undoing == Some(plan) {
                    m.status.undoing = None;
                }
                self.rescan_paths(m);
            }
            DeleterMsg::Unfinished { items, notes } => {
                notice_unfinished(&mut m.status, &items, notes);
            }
        }
        self.refresh_undo(m);
        self.refresh_status(m);
    }

    /// Applies the deleter's result for one item of the running plan.
    fn item_result(
        &mut self,
        m: &mut Model,
        plan: u64,
        node: NodeRef,
        bytes: u64,
        result: &DeleteResult,
    ) {
        let Some(active) = self.active_plan.as_mut().filter(|a| a.id == plan) else {
            return;
        };
        active.remaining.remove(&node);
        active.summary.add(node, bytes, result);
        let mode = active.mode;
        let done = active.total - active.remaining.len();
        if let Some(d) = m.status.deleting.as_mut() {
            d.done = done;
        }
        let ix = m.tree.resolve(node);
        if matches!(result, DeleteResult::Deleted) {
            if mode.undoable() {
                self.add_undoable(plan, mode, bytes);
            }
            if mode == Mode::Stage {
                m.status.staged.add(bytes);
            }
            if let Some(ix) = ix {
                m.tree.detach(ix);
                self.work.push_back(Work::Free);
            }
        } else {
            let name = ix
                .map(|ix| String::from_utf8_lossy(m.tree.name(ix)).into_owned())
                .unwrap_or_default();
            m.status.notice(format!("delete {name}: {result}"));
            if ix.is_some_and(|ix| unmark_kept(&mut m.tree, ix, result)) {
                self.deferred_rescans.push(node);
            }
        }
        self.retry_deferred_rescans(m);
    }
}

/// What the scan saw in each directory of `item` it read completely, for the deleter to
/// compare (`dfdu_clean::inside`). Linear in the item's size, under the write lock.
fn listings(tree: &Tree, item: Ix) -> dfdu_clean::inside::Listings {
    use dfdu_clean::inside::{Listing, name_hash};
    let mut out = dfdu_clean::inside::Listings::new();
    if tree.kind(item) != Kind::Dir {
        return out;
    }
    let mut stack = vec![(item, dfdu_tree::HostPath::empty(tree.host()))];
    while let Some((d, rel)) = stack.pop() {
        if tree.dir(d).is_none_or(|r| r.state != DirState::Complete) {
            continue;
        }
        let mut entries = vec![];
        for c in tree.children(d) {
            let name = tree.name(c);
            entries.push((name_hash(name), tree.dev_ino(c).1, tree.kind(c)));
            if tree.kind(c) == Kind::Dir {
                stack.push((c, rel.join(name)));
            }
        }
        entries.sort_unstable();
        out.insert(
            rel,
            Listing {
                ino: tree.dev_ino(d).1,
                entries,
            },
        );
    }
    out
}

impl DeleteSummary {
    /// Counts the result for `node`, of `bytes`.
    fn add(&mut self, node: NodeRef, bytes: u64, result: &DeleteResult) {
        match result {
            DeleteResult::Deleted => {
                self.deleted += 1;
                self.bytes += bytes;
                return;
            }
            DeleteResult::Stale(_) => self.stale += 1,
            DeleteResult::Refused(_) => self.refused += 1,
            DeleteResult::Aborted => self.aborted += 1,
            DeleteResult::Failed { .. } => self.failed += 1,
        }
        if self.problems.len() < PROBLEMS_KEPT {
            self.problems.push((node, result.to_string()));
        }
    }
}

/// Clears the deleting mark of `ix`, an item the deleter kept, and says whether it is to be
/// rescanned: a directory cut for the delete, or partly deleted, no longer has known totals.
/// It is marked cut, which keeps it shown as incomplete if that rescan is cancelled or never
/// runs.
fn unmark_kept(tree: &mut Tree, ix: Ix, result: &DeleteResult) -> bool {
    tree.set_deleting(ix, false);
    let Some(d) = tree.dir_mut(ix) else {
        return false;
    };
    let was_cut = d.cut && d.state == DirState::Deleting;
    let partial = matches!(result, DeleteResult::Failed { partial: true, .. });
    // A directory on another filesystem shows none of its contents, so a part-way delete
    // inside it leaves nothing stale.
    if !(was_cut || partial) || d.state == DirState::Skipped {
        return false;
    }
    d.cut = true;
    tree.touch(ix);
    true
}

/// Notices of the deletes an earlier session left unfinished, and of the deletion journal.
fn notice_unfinished(status: &mut Status, items: &[String], notes: Vec<String>) {
    if let Some(first) = items.first() {
        let more = match items.len() {
            1 => String::new(),
            n => format!("; and {} more", n - 1),
        };
        status.notice(format!(
            "an earlier session left {} delete{} unfinished, kept as they are \
             (dfdu recover lists them): {first}{more}",
            items.len(),
            if items.len() == 1 { "" } else { "s" },
        ));
    }
    for n in notes {
        status.notice(format!("deletion journal: {n}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dfdu_tree::ErrKind;

    #[test]
    fn a_summary_counts_each_result_and_keeps_the_problems() {
        let mut s = DeleteSummary::default();
        let n = NodeRef::from_bits;
        s.add(n(1), 100, &DeleteResult::Deleted);
        s.add(n(2), 50, &DeleteResult::Stale("changed".into()));
        s.add(n(3), 10, &DeleteResult::Aborted);
        let busy = DeleteResult::Failed {
            partial: true,
            err: ErrKind::Other,
            msg: "busy".into(),
        };
        s.add(n(4), 10, &busy);
        assert_eq!((s.deleted, s.bytes), (1, 100));
        assert_eq!((s.stale, s.refused, s.failed, s.aborted), (1, 0, 1, 1));
        let kept: Vec<NodeRef> = s.problems.iter().map(|p| p.0).collect();
        assert_eq!(kept, [n(2), n(3), n(4)]);
    }

    /// The keys of what the scan saw inside an item are joined with the separator of the
    /// tree's host, which is what the deleter there joins them with: a Windows client deleting
    /// on a Unix host must not skip the check below the item's first level.
    #[test]
    fn listings_are_keyed_with_the_hosts_separator() {
        use crate::test_util::meta;
        use dfdu_tree::{Host, HostPath};
        for host in [Host::Unix, Host::Windows] {
            let mut t = Tree::new();
            t.set_host(host);
            let root = t.new_root(b"/r", &meta(Kind::Dir, 0, 1), true);
            let item = t.add_dir(root, b"item", &meta(Kind::Dir, 0, 2));
            let a = t.add_dir(item, b"a", &meta(Kind::Dir, 0, 3));
            let b = t.add_dir(a, b"b", &meta(Kind::Dir, 0, 4));
            t.add_file(b, b"f", &meta(Kind::File, 1, 5), false, None);
            for d in [item, a, b] {
                t.dir_mut(d).expect("directory").state = DirState::Complete;
            }
            let mut keys: Vec<HostPath> = listings(&t, item).into_keys().collect();
            keys.sort();
            let sep = char::from(host.sep());
            let want =
                ["", "a", &format!("a{sep}b")].map(|k| HostPath::new(host, k.as_bytes().to_vec()));
            assert_eq!(keys, want, "{host:?}");
        }
    }

    #[test]
    fn problems_past_the_limit_are_counted_not_kept() {
        let mut s = DeleteSummary::default();
        for i in 0..PROBLEMS_KEPT as u64 + 5 {
            s.add(NodeRef::from_bits(i), 1, &DeleteResult::Aborted);
        }
        assert_eq!(s.aborted, PROBLEMS_KEPT + 5);
        assert_eq!(s.problems.len(), PROBLEMS_KEPT);
    }
}

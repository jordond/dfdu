//! What an item of a delete plan is checked against before it is queued, and what it carries
//! to the deleter.

use std::sync::Arc;

use dfdu_clean::inside::{Listing, Listings, name_hash};
use dfdu_clean::{Identity, PlanItem};
use dfdu_scan::Backend;
use dfdu_tree::{DirState, HostPath, Ix, Kind, NodeFlags, Tree};

use crate::applier::{Applier, JobKind};

impl<B: Backend> Applier<B> {
    /// Why `ix` is not deleted, said after its name: a rescan of it, an ancestor or a
    /// descendant is running, it is not refreshed yet, or the tree says it cannot be.
    pub(super) fn item_refusal(&self, tree: &Tree, ix: Ix) -> Option<String> {
        // Overlap with rescans: a running rescan of the item, an ancestor or a descendant
        // blocks it. Cancelling a descendant's rescan instead would leave that directory cut,
        // with partial totals, if this delete then did not happen.
        let blocked = self
            .jobs
            .values()
            .filter(|j| j.kind == JobKind::Rescan && j.swap_pending())
            .filter_map(|j| tree.resolve_visible(j.target))
            .any(|t| t == ix || Self::is_ancestor(tree, t, ix) || Self::is_ancestor(tree, ix, t));
        if blocked {
            return Some("or something in it is being rescanned; delete after it completes".into());
        }
        if crate::applier::unrefreshed(tree, ix) {
            return Some(
                "is not refreshed yet: loaded from a file, and no resume has taken it over".into(),
            );
        }
        if self.unverified(tree, ix) {
            return Some(
                "is not refreshed yet: its resume was cancelled before listing it again; rescan \
                 it first"
                    .into(),
            );
        }
        refusal(tree, ix, self.privileged)
    }
}

/// Why the tree says `ix` cannot be deleted; the deleter checks the filesystem again
/// before touching it.
pub(super) fn refusal(tree: &Tree, ix: Ix, privileged: bool) -> Option<String> {
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

/// The plan item of `ix`: its path, the identity the deleter checks again, its bytes and what
/// the scan saw inside it.
pub(super) fn plan_item(tree: &Tree, ix: Ix) -> PlanItem {
    let (dev, ino) = tree.dev_ino(ix);
    PlanItem {
        node: tree.node_ref(ix),
        path: tree.host_path(ix),
        identity: Identity {
            dev,
            ino,
            kind: tree.kind(ix),
            apparent: tree.raw_size(ix).0,
            mtime: tree.mtime(ix),
        },
        bytes: tree.total(ix).alloc,
        inside: Arc::new(listings(tree, ix)),
    }
}

/// What the scan saw in each directory of `item` it read completely, for the deleter to
/// compare (`dfdu_clean::inside`). Linear in the item's size, under the write lock.
pub(super) fn listings(tree: &Tree, item: Ix) -> Listings {
    let mut out = Listings::new();
    if tree.kind(item) != Kind::Dir {
        return out;
    }
    let mut stack = vec![(item, HostPath::empty(tree.host()))];
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

//! Tree mode's rows against the tree they come from, for random trees and expanded sets.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use dfdu_tree::{Ix, NodeRef};
use proptest::prelude::*;
use proptest::sample::Index;

use super::{Fx, read};
use crate::app::App;
use crate::model::snapshot::{EntryData, Snapshot};
use crate::state::order::{SortKey, SortSpec, tree_order};
use crate::state::tree::TreeState;
use crate::theme::Theme;

const SPECS: [SortSpec; 3] = [
    SortSpec {
        key: SortKey::Size,
        desc: true,
    },
    SortSpec {
        key: SortKey::Name,
        desc: false,
    },
    SortSpec {
        key: SortKey::Items,
        desc: true,
    },
];

/// The rows tree mode should show of `fx`, worked out from the tree: the nodes with their own
/// row, and the count and size of each fold row by its parent.
fn expected(fx: &Fx, t: &TreeState) -> (HashSet<NodeRef>, HashMap<NodeRef, (u32, u64)>) {
    let tree = &fx.m.tree;
    let (mut rows, mut folds) = (HashSet::new(), HashMap::new());
    let mut todo: Vec<Ix> = tree.children(fx.root).collect();
    while let Some(ix) = todo.pop() {
        let node = tree.node_ref(ix);
        rows.insert(node);
        if !t.is_expanded(node) {
            continue;
        }
        let size = tree.total(ix).alloc;
        #[expect(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "as the read works it out"
        )]
        let threshold = (size as f64 * t.fold_ratio_of(node)).ceil() as u64;
        let (mut n, mut sum) = (0, 0);
        for c in tree.children(ix) {
            let s = tree.total(c).alloc;
            if s >= threshold {
                todo.push(c);
            } else {
                n += 1;
                sum += s;
            }
        }
        if n > 0 {
            folds.insert(node, (n, sum));
        }
    }
    (rows, folds)
}

/// Checks that `rows` is a preorder of the tree the parents describe: each row comes right
/// after its parent's subtree so far, and siblings are in `spec` order with fold rows last.
fn check_preorder(es: &[EntryData], rows: &[usize], spec: SortSpec) -> Result<(), TestCaseError> {
    let mut path: Vec<usize> = Vec::new();
    let mut last_child: HashMap<u32, usize> = HashMap::new();
    for &i in rows {
        let p = es[i].parent;
        while path
            .last()
            .is_some_and(|&top| u32::try_from(top).ok() != Some(p))
        {
            path.pop();
        }
        prop_assert!(
            p == u32::MAX || !path.is_empty(),
            "row {i} does not follow its parent"
        );
        prop_assert_eq!(usize::from(es[i].depth), path.len());
        if let Some(&before) = last_child.get(&p) {
            let (a, b) = (&es[before], &es[i]);
            prop_assert!(a.fold == 0, "a fold row is last among its siblings");
            if b.fold == 0 {
                let ok = match spec.key {
                    SortKey::Size => a.size >= b.size,
                    SortKey::Items => a.items >= b.items,
                    SortKey::Name | SortKey::Mtime => a.name[..] <= b.name[..],
                };
                prop_assert!(ok, "siblings out of order: {:?} {:?}", a.name, b.name);
            }
        }
        last_child.insert(p, i);
        path.push(i);
    }
    Ok(())
}

/// Builds the tree: each node goes below one of the directories made before it.
fn build(nodes: &[(Index, bool, u64)], open: &[bool]) -> (Fx, TreeState) {
    let mut fx = Fx::new();
    let mut dirs = vec![fx.root];
    for (k, (at, is_dir, size)) in nodes.iter().enumerate() {
        let parent = dirs[at.index(dirs.len())];
        let name = format!("n{k:02}");
        if *is_dir {
            let d = fx.dir(parent, &name);
            dirs.push(d);
        } else {
            fx.file(parent, &name, *size);
        }
    }
    let mut t = TreeState::default();
    for (d, &o) in dirs.iter().skip(1).zip(open) {
        if o {
            fx.expand(&mut t, *d);
        }
    }
    (fx, t)
}

fn snapshot_of(fx: &Fx, t: &TreeState) -> Snapshot {
    read(&fx.m, t)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    #[test]
    fn rows_are_a_preorder_of_the_expanded_tree(
        nodes in prop::collection::vec((any::<Index>(), any::<bool>(), 0u64..1000), 1..40),
        open in prop::collection::vec(any::<bool>(), 40),
        ratio in prop::sample::select(vec![0.0, 0.01, 0.1, 0.3]),
        spec in prop::sample::select(SPECS.to_vec()),
    ) {
        let (fx, mut t) = build(&nodes, &open);
        t.fold_ratio = ratio;
        let snap = snapshot_of(&fx, &t);
        let es = &snap.entries;
        let rows = tree_order(es, spec);
        let mut sorted = rows.clone();
        sorted.sort_unstable();
        prop_assert_eq!(sorted, (0..es.len()).collect::<Vec<_>>(), "every entry once");
        check_preorder(es, &rows, spec)?;

        let (want_rows, want_folds) = expected(&fx, &t);
        let got_rows: HashSet<NodeRef> =
            es.iter().filter(|e| e.fold == 0).map(|e| e.node).collect();
        prop_assert_eq!(got_rows, want_rows);
        let got_folds: HashMap<NodeRef, (u32, u64)> = es
            .iter()
            .filter(|e| e.fold > 0)
            .map(|e| (es[e.parent as usize].node, (e.fold, e.size)))
            .collect();
        prop_assert_eq!(got_folds, want_folds);
        // The children of every expanded row, the fold row included, add up to it.
        for (p, e) in es.iter().enumerate() {
            let kids: Vec<&EntryData> =
                es.iter().filter(|c| c.parent as usize == p && c.parent != u32::MAX).collect();
            if !kids.is_empty() {
                prop_assert_eq!(kids.iter().map(|c| c.size).sum::<u64>(), e.size);
            }
        }
    }

    #[test]
    fn selection_keeps_its_node_across_a_resort(
        nodes in prop::collection::vec((any::<Index>(), any::<bool>(), 0u64..1000), 1..40),
        open in prop::collection::vec(any::<bool>(), 40),
        pick in any::<Index>(),
        from in prop::sample::select(SPECS.to_vec()),
        to in prop::sample::select(SPECS.to_vec()),
    ) {
        let (fx, t) = build(&nodes, &open);
        let t0 = Instant::now();
        let mut app = App::new(Theme::no_color(), t0);
        app.session.sort = from;
        let snap = snapshot_of(&fx, &t);
        app.session.tree = Some(t);
        app.refresh(snap, t0);
        prop_assert_eq!(&app.session.list.rows, &tree_order(&app.session.snap.entries, from));
        prop_assume!(!app.session.list.rows.is_empty());
        app.session.move_to(pick.index(app.session.list.rows.len()));
        let node = app.session.selected_entry().map(|e| e.node);
        app.session.sort = to;
        app.session.request_sort();
        app.reorder(t0);
        prop_assert_eq!(&app.session.list.rows, &tree_order(&app.session.snap.entries, to));
        prop_assert_eq!(app.session.selected_entry().map(|e| e.node), node);
    }
}

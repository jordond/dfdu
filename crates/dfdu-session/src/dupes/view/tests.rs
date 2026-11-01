use std::sync::{Arc, OnceLock};
use std::time::Instant;

use super::*;
use crate::test_support::Fixture;

/// A browser of the Duplicates view whose comparison found `groups`: files below `/r` with
/// their dates, each still in the tree.
fn found_in(groups: &[&[(&str, i64)]]) -> SessionState {
    let mut s = SessionState::new(Instant::now());
    let mut bits = 0;
    let mut file = |path: &str, mtime: i64| {
        bits += 1;
        s.extras
            .dupes
            .insert(path.into(), Some(NodeRef::from_bits(bits)));
        Candidate {
            path: path.into(),
            size: 1 << 20,
            alloc: 1 << 20,
            dev_ino: (1, bits),
            mtime,
            links: 0,
            private: None,
        }
    };
    let groups = (groups.iter())
        .map(|files| Group {
            size: 1 << 20,
            hash: [0; 32],
            files: files.iter().map(|&(p, mtime)| file(p, mtime)).collect(),
        })
        .collect();
    let result = Arc::new(OnceLock::from(Found {
        groups,
        compared: 4,
        ..Found::default()
    }));
    s.view = View::Duplicates;
    s.dupes.run = Some(Run {
        root: "/r".into(),
        progress: Arc::default(),
        cancel: Arc::default(),
        result,
    });
    s
}

/// `/r/a` (oldest), `/r/bb/b` and `/r/c` (newest) in one group, and `/r/x` and `/r/y` in
/// another.
fn two_groups() -> SessionState {
    found_in(&[
        &[("/r/a", 10), ("/r/bb/b", 20), ("/r/c", 30)],
        &[("/r/x", 5), ("/r/y", 5)],
    ])
}

/// Makes group `g` of the result a group of clones.
fn clones(s: &mut SessionState, g: usize) {
    let mut groups = found(s).expect("a result").groups.clone();
    for c in &mut groups[g].files {
        c.private = Some(0);
    }
    let run = s.dupes.run.as_mut().expect("a run");
    run.result = Arc::new(OnceLock::from(Found {
        groups,
        ..Found::default()
    }));
}

/// The copies a delete would take in group `g`: the ticked ones still in the tree.
fn ticked(s: &SessionState, g: usize) -> usize {
    let group = &found(s).expect("a result").groups[g];
    (group.files.iter())
        .filter_map(|c| live(&s.extras.dupes, c))
        .filter(|&n| s.marks.contains(n))
        .count()
}

fn marked_paths(s: &SessionState) -> Vec<String> {
    (s.marks.items().iter())
        .map(|i| i.target.path.to_string())
        .collect()
}

#[test]
fn open_groups_list_their_files_and_clones_come_last_closed() {
    let mut s = two_groups();
    assert_eq!(
        rows(&s),
        [
            Row::Group(0),
            Row::File(0, 0),
            Row::File(0, 1),
            Row::File(0, 2),
            Row::Group(1),
            Row::File(1, 0),
            Row::File(1, 1)
        ]
    );
    clones(&mut s, 0);
    assert_eq!(
        rows(&s),
        [
            Row::Group(1),
            Row::File(1, 0),
            Row::File(1, 1),
            Row::Group(0)
        ]
    );
    s.dupes.toggled.insert(0);
    assert_eq!(rows(&s).len(), 7, "a closed group opened");
    // A group with one file left is no group.
    s.extras.dupes.insert("/r/x".into(), None);
    assert!(!rows(&s).contains(&Row::Group(1)));
}

#[test]
fn marking_all_leaves_the_kept_copies_and_the_clones() {
    let mut s = two_groups();
    clones(&mut s, 1);
    assert_eq!(mark_all(&mut s), 2);
    assert_eq!(marked_paths(&s), ["/r/bb/b", "/r/c"]);
    assert_eq!(mark_all(&mut s), 0, "marked already");
    let item = &s.marks.items()[0];
    assert!(!item.is_dir && item.items == 1);
    assert_eq!(item.size, 1 << 20, "what deleting it frees");
}

/// Ticking the suggested copies, then the next keep rule, then ticking again never ticks every
/// copy: the copy the new rule keeps is unticked when the rule changes.
#[test]
fn the_kept_copy_is_never_ticked_by_tick_suggested_and_cycle_keep() {
    let mut s = found_in(&[&[
        ("/r/new.bin", 1_760_000_000),
        ("/r/a/old.bin", 1_700_000_000),
    ]]);
    for keys in ["A", "AKA", "AKAKA", "KAKAKA"] {
        crate::marks::clear(&mut s);
        for k in keys.chars() {
            let intent = if k == 'A' {
                Intent::TickSuggested
            } else {
                Intent::CycleKeep
            };
            let _ = act(&mut s, intent);
            assert!(ticked(&s, 0) < 2, "{keys}: every copy is ticked");
        }
        assert_eq!(ticked(&s, 0), 1, "{keys}");
    }
}

/// Toggle on a file ticks it and moves down; on a group, every copy but the one kept. A
/// read-only browser ticks nothing and says why.
#[test]
fn toggle_ticks_a_file_or_a_groups_copies_but_not_read_only() {
    let mut s = two_groups();
    let _ = act(&mut s, Intent::Select(1));
    let _ = act(&mut s, Intent::Toggle);
    assert_eq!((marked_paths(&s), s.dupes.cursor), (vec!["/r/a".into()], 2));
    crate::marks::clear(&mut s);
    let _ = act(&mut s, Intent::Select(0));
    let _ = act(&mut s, Intent::Toggle);
    assert_eq!(marked_paths(&s), ["/r/bb/b", "/r/c"]);
    crate::marks::clear(&mut s);
    s.read_only = true;
    for row in [0, 1] {
        let _ = act(&mut s, Intent::Select(row));
        s.status = None;
        let _ = act(&mut s, Intent::Toggle);
        assert!(s.marks.is_empty(), "row {row}");
        assert_eq!(s.status.as_deref(), Some(READ_ONLY), "row {row}");
    }
}

/// Activate opens and closes a group and shows a file in Files; CloseUp closes the group the
/// cursor is in.
#[test]
fn groups_open_and_close_and_files_show_in_files() {
    let mut s = two_groups();
    let _ = act(&mut s, Intent::Select(2));
    let show = act(&mut s, Intent::Activate).show.expect("shown in Files");
    assert_eq!(
        (show.path.to_string(), show.root.to_string()),
        ("/r/bb/b".into(), "/r".into())
    );
    let _ = act(&mut s, Intent::CloseUp);
    assert_eq!(s.dupes.cursor, 0);
    assert_eq!(rows(&s).len(), 4, "the first group closed");
    let _ = act(&mut s, Intent::Activate);
    assert_eq!(rows(&s).len(), 7, "and open again");
    assert_eq!(selected(&s), None, "a group has no path");
    let _ = act(&mut s, Intent::Select(3));
    assert_eq!(selected(&s), Some("/r/c".into()));
}

/// Comparing again while a scan runs keeps the comparison shown: another cannot start yet.
#[test]
fn compare_again_while_scanning_keeps_what_is_shown() {
    let mut s = found_in(&[&[("/r/a", 1), ("/r/b", 1)]]);
    s.outside.local = true;
    s.snap.busy = true;
    assert!(act(&mut s, Intent::CompareAgain).effects.is_empty());
    assert!(found(&s).is_some(), "the result is kept");
    let status = s.status.clone().unwrap_or_default();
    assert!(status.contains("wait until the scan is done"), "{status}");
    // Done, it stops the one shown and asks for the candidates.
    s.snap.busy = false;
    let cancel = s.dupes.run.as_ref().map(|r| r.cancel.clone());
    let then = act(&mut s, Intent::CompareAgain);
    assert!(matches!(then.effects[..], [Effect::FindDupes]));
    assert!(cancel.is_some_and(|c| c.load(Ordering::Relaxed)));
    assert!(s.dupes.run.is_none() && s.extras.dupes.is_empty());
}

/// Before comparing this machine's files, the view counts the files a comparison would read,
/// once per change of the tree; a scan from elsewhere counts nothing and compares nothing.
#[test]
fn the_files_to_compare_are_counted_once() {
    let (mut fx, root) = Fixture::new();
    let big = 2 * min_size();
    for name in ["a.iso", "b.iso", "c.iso"] {
        fx.file(root, name, big);
    }
    fx.file(root, "other.bin", big + 1);
    let mut s = SessionState::new(Instant::now());
    s.view = View::Duplicates;
    assert!(
        !stale(&s) && count(&fx.m, &s).is_none(),
        "not this machine's"
    );
    let then = act(&mut s, Intent::Compare);
    assert!(then.effects.is_empty());
    assert!(s.status.as_deref().is_some_and(|st| st.contains("remote")));
    s.outside.local = true;
    assert!(stale(&s), "the files to compare are counted");
    s.extras.dupe_candidates = count(&fx.m, &s);
    apply(&mut s);
    assert_eq!(s.dupes.candidates, Some(3));
    assert!(!stale(&s));
    assert_eq!(count(&fx.m, &s), None, "counted once for this tree");
    let then = act(&mut s, Intent::Compare);
    assert!(matches!(then.effects[..], [Effect::FindDupes]));
}

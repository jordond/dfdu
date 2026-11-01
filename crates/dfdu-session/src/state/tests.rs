//! Helpers for the tests of the logic on a [`SessionState`]: a browser holding a directory
//! with its rows sorted, as a front end has it after a refresh.

use std::time::Instant;

use dfdu_tree::HostPath;

use super::{SessionState, shown};
use crate::snapshot::{EntryData, Snapshot, test_entry};

/// A browser of `/r` holding `entries`, its rows sorted and the first one selected.
pub(crate) fn browser(entries: Vec<EntryData>) -> SessionState {
    let mut s = SessionState::new(Instant::now());
    s.snap = Snapshot {
        dir_path: HostPath::from("/r"),
        dir_node: Some(test_entry(99, "/r", 0).node),
        at_root: true,
        entries,
        ..Default::default()
    };
    sort(&mut s);
    s.select(0);
    s
}

/// Sorts the rows again, as a front end's refresh does with an exact sort.
pub(crate) fn sort(s: &mut SessionState) {
    s.filter.prepare(&s.snap, true);
    let (hide, filter) = (s.hide_hidden, &s.filter);
    let visible = |i: usize, e: &EntryData| shown(hide, e) && filter.matches(i);
    s.list.order = crate::order::Order::default();
    s.list.order.sort_exact(&s.snap.entries, visible, s.sort);
    s.list.rows = s.list.order.rows().to_vec();
}

/// The names of the marked entries, in the order they were marked.
pub(crate) fn marked_names(s: &SessionState) -> Vec<String> {
    (s.marks.items().iter())
        .map(|it| {
            String::from_utf8_lossy(it.target.path.file_name().unwrap_or_default()).into_owned()
        })
        .collect()
}

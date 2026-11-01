//! The view's rows and what a tick takes: the groups with two files left, each open one
//! followed by its files, the copy the keep rule keeps, and the copies the others are.

use dfdu_dupes::{Candidate, Group};
use dfdu_tree::{HostPath, Ident, Kind, NodeRef, Target};

use super::found;
use crate::app::App;
use crate::state::marks;

/// A row of the view: a group, or a file of one by its index in the group.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum Row {
    Group(usize),
    File(usize, usize),
}

/// A file still in the tree.
pub(super) fn live(app: &App, c: &Candidate) -> Option<NodeRef> {
    dfdu_session::dupes::live(&app.extras.dupes, c)
}

/// Whether group `g` shows its files: by default unless it is clones, which free nothing.
pub(super) fn open(app: &App, g: usize, group: &Group) -> bool {
    !group.all_clones() ^ app.dupes.toggled.contains(&g)
}

/// The groups with at least two files left, clones last; each open one followed by its files.
pub(super) fn rows(app: &App) -> Vec<Row> {
    let Some(f) = found(app) else {
        return Vec::new();
    };
    let mut order: Vec<usize> = (0..f.groups.len()).collect();
    order.sort_by_key(|&g| f.groups[g].all_clones());
    let mut out = Vec::new();
    for g in order {
        let group = &f.groups[g];
        let left: Vec<usize> = (0..group.files.len())
            .filter(|&i| live(app, &group.files[i]).is_some())
            .collect();
        if left.len() < 2 {
            continue;
        }
        out.push(Row::Group(g));
        if open(app, g, group) {
            out.extend(left.into_iter().map(|i| Row::File(g, i)));
        }
    }
    out
}

pub(super) fn mark_of(c: &Candidate, node: NodeRef) -> marks::Item {
    marks::Item {
        target: Target {
            node: Some(node),
            path: HostPath::local(&c.path),
            ident: Some(Ident {
                dev_ino: c.dev_ino,
                kind: Kind::File,
            }),
        },
        is_dir: false,
        size: c.freed(),
        items: 1,
        shared_out: 0,
        scanning: false,
        inside: false,
    }
}

/// The index in `group` of the file still in the tree that the keep rule keeps
/// ([`dfdu_session::dupes::kept`]).
pub(super) fn kept(app: &App, group: &Group) -> Option<usize> {
    dfdu_session::dupes::kept(group, app.dupes.keep, &app.extras.dupes)
}

/// The files of `group` still in the tree, but the one [`kept`].
pub(super) fn copies(app: &App, group: &Group) -> Vec<(Candidate, NodeRef)> {
    dfdu_session::dupes::copies(group, app.dupes.keep, &app.extras.dupes)
}

/// Marks every copy but the one kept in each group that frees something
/// ([`dfdu_session::dupes::suggested`]); returns how many were newly marked.
pub(super) fn mark_all(app: &mut App) -> usize {
    let Some(f) = found(app) else {
        return 0;
    };
    let picks: Vec<(Candidate, NodeRef)> =
        dfdu_session::dupes::suggested(f, app.dupes.keep, &app.extras.dupes)
            .into_iter()
            .filter(|(_, n)| !app.marks.contains(*n))
            .collect();
    for (c, n) in &picks {
        marks::set(app, mark_of(c, *n), true);
    }
    picks.len()
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, OnceLock};
    use std::time::Instant;

    use dfdu_dupes::Found;

    use super::*;
    use crate::features::dupes::tests::found_in;
    use crate::theme::Theme;

    /// A browser whose comparison found `/r/a` (oldest), `/r/bb/b` and `/r/c` (newest) in one
    /// group, and `/r/x` and `/r/y` in another.
    fn app() -> App {
        let mut app = App::new(Theme::no_color(), Instant::now());
        found_in(
            &mut app,
            &[
                &[("/r/a", 10), ("/r/bb/b", 20), ("/r/c", 30)],
                &[("/r/x", 5), ("/r/y", 5)],
            ],
        );
        app
    }

    /// Makes group `g` of the result a group of clones.
    fn clones(app: &mut App, g: usize) {
        let mut groups = found(app).expect("a result").groups.clone();
        for c in &mut groups[g].files {
            c.private = Some(0);
        }
        let run = app.dupes.run.as_mut().expect("a run");
        run.result = Arc::new(OnceLock::from(Found {
            groups,
            ..Found::default()
        }));
    }

    #[test]
    fn open_groups_list_their_files_and_clones_come_last_closed() {
        let mut app = app();
        assert_eq!(
            rows(&app),
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
        clones(&mut app, 0);
        assert_eq!(
            rows(&app),
            [
                Row::Group(1),
                Row::File(1, 0),
                Row::File(1, 1),
                Row::Group(0)
            ]
        );
        app.dupes.toggled.insert(0);
        assert_eq!(rows(&app).len(), 7, "a closed group opened");
        // A group with one file left is no group.
        app.extras.dupes.insert("/r/x".into(), None);
        assert!(!rows(&app).contains(&Row::Group(1)));
    }

    #[test]
    fn marking_all_leaves_the_kept_copies_and_the_clones() {
        let mut app = app();
        clones(&mut app, 1);
        assert_eq!(mark_all(&mut app), 2);
        let local = |p: &str| HostPath::local(std::path::Path::new(p));
        assert_eq!(
            crate::tests::marked_paths(&app),
            [local("/r/bb/b"), local("/r/c")]
        );
        assert_eq!(mark_all(&mut app), 0, "marked already");
        let item = &app.marks.items()[0];
        assert!(!item.is_dir && item.items == 1);
        assert_eq!(item.size, 1 << 20, "what deleting it frees");
    }
}

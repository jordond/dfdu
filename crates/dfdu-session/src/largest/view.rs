//! The Largest view: only while it is open, the extras read walks the current directory's
//! subtree under the read lock ([`super::walk`]). It walks again when the view opens, for
//! another directory or size mode, and when the scan data changed: at once when no scan is
//! running (a delete), and during a scan at most every [`MIN_INTERVAL`] and at most a tenth of
//! the time. Ticks are the marks.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use dfdu_engine::{Model, SizeMode};
use dfdu_tree::{HostPath, Ident, Kind, NodeRef, Target};

use super::{File, walk};
use crate::marks;
use crate::state::SessionState;
use crate::view::{Then, View};

/// During a scan, the walk runs at most this often.
pub const MIN_INTERVAL: Duration = Duration::from_secs(1);

/// What the view keeps between frames.
#[derive(Clone, Debug, Default)]
pub struct Largest {
    /// The selected file.
    pub cursor: usize,
    /// The files the last walk found, largest first.
    pub files: Vec<File>,
    /// The files below the directory, all of them, as the last walk counted them.
    pub count: u64,
    /// What the last walk was for; `None` until a walk for the view as opened came in.
    pub walked: Option<Walked>,
}

/// What a walk was for, and when it ran.
#[derive(Clone, Debug)]
pub struct Walked {
    dir: HostPath,
    mode: SizeMode,
    hide_hidden: bool,
    at: Instant,
    /// How long it held the read lock.
    cost: Duration,
    /// The model's fingerprint then.
    fingerprint: u64,
}

/// What [`read`] found.
#[derive(Clone, Debug)]
pub struct Info {
    files: Vec<File>,
    count: u64,
    walked: Walked,
}

/// What a front end's keys ask of the view.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Intent {
    /// Puts the cursor on file `n`.
    Select(usize),
    /// Shows the selected file in Files, in its directory.
    Activate,
    /// Ticks or unticks the selected file and moves down a row.
    Toggle,
}

/// True when the view is shown, and so takes the intents it knows.
pub fn active(s: &SessionState) -> bool {
    s.current_view() == View::Largest
}

/// The view was opened: it walks again before the next frame, so what it shows is never older
/// than the opening.
pub fn on_open(s: &mut SessionState) {
    s.largest.walked = None;
}

/// The view needs a walk at once: it was just opened, or shows another directory, size mode
/// or choice of hidden files than the last walk was for.
pub fn stale(s: &SessionState) -> bool {
    active(s)
        && !s.largest.walked.as_ref().is_some_and(|w| {
            w.dir == s.snap.dir_path && w.mode == s.size_mode && w.hide_hidden == s.hide_hidden
        })
}

/// Under the read lock, while the view is open and a walk is due: the largest files below the
/// current directory ([`walk`]). `None` keeps what the last walk found.
pub fn read(m: &Model, s: &SessionState) -> Option<Info> {
    if !active(s) {
        return None;
    }
    let fingerprint = m.fingerprint();
    if !stale(s) {
        let w = s.largest.walked.as_ref()?;
        let paced = !s.snap.busy || w.at.elapsed() >= MIN_INTERVAL.max(w.cost * 9);
        if w.fingerprint == fingerprint || !paced {
            return None;
        }
    }
    let at = Instant::now();
    let t = &m.tree;
    let (files, count) = match s.snap.dir_node.and_then(|r| t.resolve_visible(r)) {
        Some(dir) => walk(t, dir, s.size_mode, s.hide_hidden),
        None => (Vec::new(), 0),
    };
    Some(Info {
        files,
        count,
        walked: Walked {
            dir: s.snap.dir_path.clone(),
            mode: s.size_mode,
            hide_hidden: s.hide_hidden,
            at,
            cost: at.elapsed(),
            fingerprint,
        },
    })
}

/// Takes in a walk, now in `s.extras.largest`, keeping the cursor on the file it was on when
/// that file is still listed, and at the top for another directory or a view just opened.
pub fn apply(s: &mut SessionState) {
    let Some(info) = s.extras.largest.take() else {
        return;
    };
    let l = &mut s.largest;
    let same_dir = (l.walked.as_ref()).is_some_and(|w| w.dir == info.walked.dir);
    let was = (l.files.get(l.cursor).map(|f| f.node)).filter(|_| same_dir);
    if !same_dir {
        l.cursor = 0;
    }
    l.files = info.files;
    l.count = info.count;
    l.walked = Some(info.walked);
    l.cursor = was
        .and_then(|n| l.files.iter().position(|f| f.node == n))
        .unwrap_or_else(|| l.cursor.min(l.files.len().saturating_sub(1)));
}

/// The selected file, its path joined to the directory shown.
pub fn selected(s: &SessionState) -> Option<(HostPath, &File)> {
    let f = s.largest.files.get(s.largest.cursor)?;
    Some((s.snap.dir_path.join(f.path.as_bytes()), f))
}

/// The nodes of the files the view lists, for a delete to act on its ticks only.
pub fn listed(s: &SessionState) -> HashSet<NodeRef> {
    s.largest.files.iter().map(|f| f.node).collect()
}

/// Carries out `intent`.
pub fn act(s: &mut SessionState, intent: Intent) -> Then {
    match intent {
        Intent::Select(at) => s.largest.cursor = at,
        Intent::Activate => {
            if let Some((path, _)) = selected(s) {
                return Then::show(path, s.snap.dir_path.clone());
            }
        }
        Intent::Toggle => toggle(s),
    }
    Then::nothing()
}

/// Marks or unmarks the selected file and moves down a row.
fn toggle(s: &mut SessionState) {
    let Some((path, f)) = selected(s) else {
        return;
    };
    let item = marks::Item {
        target: Target {
            node: Some(f.node),
            path,
            ident: Some(Ident {
                dev_ino: f.dev_ino,
                kind: Kind::File,
            }),
        },
        is_dir: false,
        size: f.size,
        items: 1,
        shared_out: f.shared_out,
        scanning: false,
        inside: false,
    };
    let on = !s.marks.contains(f.node);
    marks::set(s, item, on);
    let last = s.largest.files.len().saturating_sub(1);
    s.largest.cursor = (s.largest.cursor + 1).min(last);
}

#[cfg(test)]
mod tests {
    use dfdu_engine::test_support::{add_dir, add_file};

    use super::*;
    use crate::state::READ_ONLY;
    use crate::state::tests::{opened, sync};
    use crate::test_support::root_model;

    /// `/r` with files at three depths, the largest in a hidden directory:
    /// `.cache/huge` 9000, `a/big.iso` 5000, `top.txt` 4000, `a/deep/x.bin` 3000.
    fn model() -> Model {
        let (mut m, root) = root_model();
        let a = add_dir(&mut m.tree, root, "a", 2);
        let deep = add_dir(&mut m.tree, a, "deep", 3);
        let cache = add_dir(&mut m.tree, root, ".cache", 4);
        add_file(&mut m.tree, deep, "x.bin", 3000, 10);
        add_file(&mut m.tree, a, "big.iso", 5000, 11);
        add_file(&mut m.tree, root, "top.txt", 4000, 12);
        add_file(&mut m.tree, cache, "huge", 9000, 13);
        m
    }

    fn paths(s: &SessionState) -> Vec<String> {
        (s.largest.files.iter())
            .map(|f| f.path.to_string().replace('\\', "/"))
            .collect()
    }

    /// The walk runs only while the view is open, and again only when the data changed.
    #[test]
    fn walks_only_while_open_and_when_something_changed() {
        let mut m = model();
        let (mut s, _) = opened(&m, View::Files);
        assert!(read(&m, &s).is_none(), "closed: no walk");
        s.view = View::Largest;
        on_open(&mut s);
        assert!(stale(&s), "opening asks for a walk");
        assert!(read(&m, &s).is_some());
        sync(&mut s, &m);
        assert_eq!(s.largest.count, 4);
        assert!(read(&m, &s).is_none(), "nothing changed");
        let root = m.tree.roots().next().expect("root");
        add_file(&mut m.tree, root, "new.bin", 20_000, 99);
        sync(&mut s, &m);
        assert_eq!(paths(&s)[0], "new.bin", "no scan runs: walked at once");
        // Hiding hidden files walks again without them.
        s.hide_hidden = true;
        assert!(stale(&s));
        sync(&mut s, &m);
        assert_eq!(
            paths(&s),
            ["new.bin", "a/big.iso", "top.txt", "a/deep/x.bin"]
        );
    }

    /// Toggle ticks the selected file and moves down; Activate shows it in Files. A read-only
    /// browser marks nothing.
    #[test]
    fn toggle_ticks_and_activate_shows_in_files() {
        let m = model();
        let (mut s, _) = opened(&m, View::Largest);
        let _ = act(&mut s, Intent::Toggle);
        let huge = HostPath::from("/r").join(b".cache").join(b"huge");
        assert_eq!(s.marks.items()[0].target.path, huge);
        assert_eq!(s.largest.cursor, 1);
        let _ = act(&mut s, Intent::Select(3));
        let show = act(&mut s, Intent::Activate).show.expect("shown in Files");
        assert_eq!(
            show.path,
            HostPath::from("/r").join(b"a").join(b"deep").join(b"x.bin")
        );
        assert_eq!(show.root, HostPath::from("/r"));
        crate::marks::clear(&mut s);
        s.read_only = true;
        let _ = act(&mut s, Intent::Toggle);
        assert!(s.marks.is_empty());
        assert_eq!(s.status.as_deref(), Some(READ_ONLY));
    }
}

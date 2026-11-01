//! The five views as one list, and which one is shown ([`SessionState::view`]). Files, Map and
//! Largest show the current directory; Cleanup and Duplicates cover the whole scan
//! ([`View::whole_scan`]).
//!
//! [`switch`] is the one place that changes the view, so every way to switch does it the same
//! way, and [`step`] goes round the views in order.
//!
//! An intent of a view can leave the front end something to do ([`Then`]): carry out effects,
//! or show a path in Files, which moves the front end's scroll too.

use dfdu_tree::HostPath;

use crate::actions::Action;
use crate::effect::Effect;
use crate::state::{Moved, SessionState};

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum View {
    #[default]
    Files,
    Map,
    Cleanup,
    Duplicates,
    Largest,
}

impl View {
    /// In the order of their keys and tabs.
    pub const ALL: [View; 5] = [
        View::Files,
        View::Map,
        View::Cleanup,
        View::Duplicates,
        View::Largest,
    ];

    /// The name on its tab, in help and in the footer.
    pub fn name(self) -> &'static str {
        match self {
            View::Files => "Files",
            View::Map => "Map",
            View::Cleanup => "Cleanup",
            View::Duplicates => "Duplicates",
            View::Largest => "Largest",
        }
    }

    /// The registry action that switches to it, whose key its tab shows.
    pub fn action(self) -> Action {
        match self {
            View::Files => Action::ViewFiles,
            View::Map => Action::ViewMap,
            View::Cleanup => Action::ViewCleanup,
            View::Duplicates => Action::ViewDupes,
            View::Largest => Action::ViewLargest,
        }
    }

    /// It covers the whole scan rather than the current directory.
    pub fn whole_scan(self) -> bool {
        matches!(self, View::Cleanup | View::Duplicates)
    }

    /// Its place in [`View::ALL`].
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|&v| v == self).unwrap_or(0)
    }
}

impl SessionState {
    /// The view shown. The map counts while it is on, also when the front end has no room for
    /// it and draws the list in its place. Cleanup needs the rules: without them it is Files.
    pub fn current_view(&self) -> View {
        match self.view {
            View::Cleanup if self.snap.rules.is_none() => View::Files,
            view => view,
        }
    }

    /// The tree mode state while the rows are a tree: tree mode is on and the treemap, whose
    /// tiles are the rows of one directory, is off.
    pub fn tree_shown(&self) -> Option<&crate::tree::TreeState> {
        self.tree.as_ref().filter(|_| self.view != View::Map)
    }
}

/// Switches to `view`, and opens it: Largest walks again, Cleanup reads its matches again (and,
/// switched to from another view, asks for the native sizes), and the map switched to from the
/// tree opens on the directory the selected row is in. Leaving Duplicates stops a comparison
/// that runs ([`crate::dupes::cancel`]). Cleanup without rules says so and changes nothing
/// else.
pub fn switch(s: &mut SessionState, view: View) -> Then {
    if view == View::Cleanup && s.snap.rules.is_none() {
        s.status = Some("no rules: the cleanup view needs classification".into());
        return Then::nothing();
    }
    // Before the switch, while the rows are still the tree's.
    let moved = if view == View::Map {
        crate::tree::act::treemap_dir(s)
    } else {
        None
    };
    let again = s.view == view;
    if s.view == View::Duplicates && !again && crate::dupes::cancel(s) {
        s.status = Some("duplicates: comparison stopped".into());
    }
    s.view = view;
    let effects = match view {
        View::Files | View::Map | View::Duplicates => Vec::new(),
        View::Cleanup => crate::cleanup::on_open(s, again),
        View::Largest => {
            crate::largest::on_open(s);
            Vec::new()
        }
    };
    Then {
        effects,
        show: None,
        moved,
    }
}

/// The view `by` places before or after the current one, round the end, switched to. Cleanup
/// is passed over without rules, as it cannot be shown.
pub fn step(s: &mut SessionState, by: isize) -> Then {
    let n = View::ALL.len();
    let mut at = s.current_view().index();
    for _ in 0..n {
        at = (at + n).saturating_add_signed(by) % n;
        let view = View::ALL[at];
        if view != View::Cleanup || s.snap.rules.is_some() {
            return switch(s, view);
        }
    }
    Then::nothing()
}

/// What an intent of a view leaves to the front end.
#[derive(Debug, Default)]
#[must_use]
pub struct Then {
    /// For the front end to carry out ([`crate::effect::carry_out`]).
    pub effects: Vec<Effect>,
    /// Show this path in Files: switch to Files, open the path's directory and select it.
    pub show: Option<Show>,
    /// The browser went to another directory, with its entry to show selected: the front end
    /// keeps its scroll of the directory left and shows the one opened from the top.
    pub moved: Option<Moved>,
}

/// A path to show in Files, below `root` (the scan root, or the directory the path is
/// relative to), as [`SessionState::go_to`] takes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Show {
    pub path: HostPath,
    pub root: HostPath,
}

impl Then {
    /// Nothing more to do.
    pub fn nothing() -> Self {
        Then::default()
    }

    /// Effects to carry out.
    pub fn effects(effects: Vec<Effect>) -> Self {
        Then {
            effects,
            ..Then::default()
        }
    }

    /// `path` to show in Files, below `root`.
    pub fn show(path: HostPath, root: HostPath) -> Self {
        Then {
            show: Some(Show { path, root }),
            ..Then::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::test_entry;
    use crate::state::tests::browser;

    #[test]
    fn without_rules_cleanup_is_neither_switched_to_nor_stepped_to() {
        let mut s = browser(vec![test_entry(0, "a", 10)]);
        assert!(switch(&mut s, View::Cleanup).effects.is_empty());
        assert_eq!(s.view, View::Files);
        assert!((s.status.as_deref()).is_some_and(|m| m.starts_with("no rules")));
        let mut seen = Vec::new();
        for by in [1, 1, -1, -1, -1] {
            let _ = step(&mut s, by);
            seen.push(s.view);
        }
        assert_eq!(
            seen,
            [
                View::Map,
                View::Duplicates,
                View::Map,
                View::Files,
                View::Largest
            ]
        );
        s.view = View::Cleanup;
        assert_eq!(s.current_view(), View::Files);
    }
}

//! The five views as one list, and which one is shown ([`SessionState::view`]). Files, Map and
//! Largest show the current directory; Cleanup and Duplicates cover the whole scan
//! ([`View::whole_scan`]).
//!
//! An intent of a view can leave the front end something to do ([`Then`]): carry out effects,
//! or show a path in Files, which moves the front end's scroll too.

use dfdu_tree::HostPath;

use crate::actions::Action;
use crate::effect::Effect;
use crate::state::SessionState;

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

/// What an intent of a view leaves to the front end.
#[derive(Debug, Default)]
#[must_use]
pub struct Then {
    /// For the front end to carry out.
    pub effects: Vec<Effect>,
    /// Show this path in Files: switch to Files, open the path's directory and select it.
    pub show: Option<Show>,
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
            show: None,
        }
    }

    /// `path` to show in Files, below `root`.
    pub fn show(path: HostPath, root: HostPath) -> Self {
        Then {
            effects: Vec::new(),
            show: Some(Show { path, root }),
        }
    }
}

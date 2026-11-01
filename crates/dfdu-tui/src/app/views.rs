//! The five views (`1`-`5`) as one list: which one is shown, switching between them, and what
//! the header's tab bar says of each. The view shown is one field, [`App::view`], and
//! [`switch`] is the one place that changes it, so `1`-`5`, `[`/`]`, `Esc` and a click on a tab
//! switch the same way.
//!
//! Files, Map and Largest show the current directory; Cleanup and Duplicates cover the whole
//! scan ([`View::whole_scan`]), which the header's scope line says.

use super::keys::Action;
use super::{App, Effect};
use crate::features::{cleanup, dupes, largest, treemap};
use crate::format::human;
use crate::state::tree;

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

    fn index(self) -> usize {
        Self::ALL.iter().position(|&v| v == self).unwrap_or(0)
    }
}

/// The view shown. The map counts while it is on, also when the terminal is too small for it
/// and the list is drawn in its place. Cleanup needs the rules: without them it is Files.
pub fn current(app: &App) -> View {
    match app.view {
        View::Cleanup if app.session.snap.rules.is_none() => View::Files,
        view => view,
    }
}

/// The list's rows are drawn, where the range mark and inverting the marks work: in Files, and
/// in Map while the last frame had no room for the map.
pub fn list_shown(app: &App) -> bool {
    match current(app) {
        View::Files => true,
        View::Map => !treemap::active(app),
        View::Cleanup | View::Duplicates | View::Largest => false,
    }
}

/// Switches to `view`, and opens it: Largest walks again, Cleanup reads its matches again (and,
/// switched to from another view, asks for the native sizes), and the map switched to from the
/// tree opens on the directory the selected row is in. Cleanup without rules says so and
/// changes nothing else.
pub fn switch(app: &mut App, view: View) -> Vec<Effect> {
    if view == View::Cleanup && app.session.snap.rules.is_none() {
        app.session.status = Some("no rules: the cleanup view needs classification".into());
        return Vec::new();
    }
    if view == View::Map {
        // Before the switch, while the rows are still the tree's.
        tree::act::treemap_dir(app);
    }
    let again = app.view == view;
    app.view = view;
    match view {
        View::Files | View::Duplicates => Vec::new(),
        View::Map => {
            treemap::on_open(app);
            Vec::new()
        }
        View::Cleanup => cleanup::on_open(app, again),
        View::Largest => {
            largest::on_open(app);
            Vec::new()
        }
    }
}

/// `[` and `]`: the view `by` places before or after the current one, round the end. Cleanup
/// is passed over without rules, as it cannot be shown.
pub fn step(app: &mut App, by: isize) -> Vec<Effect> {
    let n = View::ALL.len();
    let mut at = current(app).index();
    for _ in 0..n {
        at = (at + n).saturating_add_signed(by) % n;
        let view = View::ALL[at];
        if view != View::Cleanup || app.session.snap.rules.is_some() {
            return switch(app, view);
        }
    }
    Vec::new()
}

/// What the scope line adds after `whole scan: <root>` for `view`: which files Duplicates
/// compares.
pub fn scope_note(view: View) -> Option<String> {
    (view == View::Duplicates).then(|| format!("files of {} or more", human(dupes::min_size())))
}

/// What a tab adds after its name: Cleanup's reclaimable total once a read of its matches came
/// in, and the number of groups of duplicates once a comparison finished, `…` while one runs.
pub fn badge(app: &App, view: View) -> Option<String> {
    match view {
        View::Cleanup => cleanup::reclaimable(app).filter(|&b| b > 0).map(short_size),
        View::Duplicates => dupes::groups(app).map(|n| match n {
            dupes::Groups::Found(n) => crate::format::count(n as u64),
            dupes::Groups::Running => "…".into(),
        }),
        View::Files | View::Map | View::Largest => None,
    }
}

/// A size in as few cells as a tab has room for: `3.1G` for `3.1 GiB`, `512B` for `512 B`.
fn short_size(bytes: u64) -> String {
    let long = human(bytes);
    match long.split_once(' ') {
        Some((n, unit)) => format!("{n}{}", unit.trim_end_matches("iB")),
        None => long,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use ratatui::crossterm::event::KeyCode;

    use super::*;
    use crate::tests::classified_app;
    use crate::tests::{library_snap, press};
    use crate::theme::Theme;

    /// The views `keys` lead to, one after each key, from Files.
    fn after(app: &mut App, keys: &[KeyCode]) -> Vec<View> {
        let t0 = Instant::now();
        (keys.iter())
            .map(|&k| {
                app.key(press(k), t0);
                current(app)
            })
            .collect()
    }

    #[test]
    fn brackets_step_through_the_views_round_the_ends() {
        let (_fx, mut app) = classified_app(Instant::now());
        let next = [KeyCode::Char(']'); 5];
        assert_eq!(
            after(&mut app, &next),
            [
                View::Map,
                View::Cleanup,
                View::Duplicates,
                View::Largest,
                View::Files
            ]
        );
        let prev = [KeyCode::Char('['); 2];
        assert_eq!(after(&mut app, &prev), [View::Largest, View::Duplicates]);
    }

    #[test]
    fn without_rules_the_brackets_pass_over_cleanup() {
        let t0 = Instant::now();
        let mut app = App::new(Theme::no_color(), t0);
        app.refresh(library_snap(), t0);
        let keys = [KeyCode::Char(']'), KeyCode::Char(']'), KeyCode::Char('[')];
        assert_eq!(
            after(&mut app, &keys),
            [View::Map, View::Duplicates, View::Map]
        );
    }

    #[test]
    fn esc_goes_back_to_files_once_nothing_is_left_to_cancel() {
        let t0 = Instant::now();
        let mut app = App::new(Theme::no_color(), t0);
        app.refresh(library_snap(), t0);
        for key in ['2', '4', '5'] {
            app.key(press(KeyCode::Char(key)), t0);
            assert_ne!(current(&app), View::Files);
            app.key(press(KeyCode::Esc), t0);
            assert_eq!(current(&app), View::Files, "from {key}");
        }
        // A kept filter is cleared first; the next Esc goes back.
        app.key(press(KeyCode::Char('2')), t0);
        for c in "/lib".chars() {
            app.key(press(KeyCode::Char(c)), t0);
        }
        app.key(press(KeyCode::Enter), t0);
        app.reorder(t0);
        assert!(app.session.filter.active());
        app.key(press(KeyCode::Esc), t0);
        app.reorder(t0);
        assert!(!app.session.filter.active());
        assert_eq!(current(&app), View::Map);
        app.key(press(KeyCode::Esc), t0);
        assert_eq!(current(&app), View::Files);
    }

    #[test]
    fn cleanup_without_rules_is_neither_switched_to_nor_shown() {
        let t0 = Instant::now();
        let mut app = App::new(Theme::no_color(), t0);
        app.refresh(library_snap(), t0);
        assert!(switch(&mut app, View::Cleanup).is_empty());
        assert_eq!(app.view, View::Files);
        assert!(
            app.session
                .status
                .as_deref()
                .is_some_and(|s| s.starts_with("no rules"))
        );
        app.view = View::Cleanup;
        assert_eq!(current(&app), View::Files);
    }

    #[test]
    fn short_sizes_drop_the_space_and_the_binary_suffix() {
        assert_eq!(short_size(512), "512B");
        assert_eq!(short_size(3 * (1 << 30) + (1 << 30) / 10), "3.1G");
        assert_eq!(short_size(1 << 20), "1.0M");
    }
}

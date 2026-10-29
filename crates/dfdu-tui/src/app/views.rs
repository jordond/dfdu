//! The five views (`1`-`5`) as one list: which one is shown, switching between them, and what
//! the header's tab bar says of each. The views keep their own on/off state in their feature
//! modules; this module is the one place that turns one on and the others off, so `1`-`5`,
//! `[`/`]`, `Esc` and a click on a tab switch the same way.
//!
//! Files, Map and Largest show the current directory; Cleanup and Duplicates cover the whole
//! scan ([`View::whole_scan`]), which the header's scope line says.

use super::keys::Action;
use super::{App, Effect};
use crate::features::{cleanup, dupes, largest, treemap};
use crate::state::tree;
use crate::theme::human;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum View {
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
/// and the list is drawn in its place.
pub fn current(app: &App) -> View {
    if largest::active(app) {
        View::Largest
    } else if dupes::active(app) {
        View::Duplicates
    } else if cleanup::active(app) {
        View::Cleanup
    } else if app.treemap.on {
        View::Map
    } else {
        View::Files
    }
}

/// Switches to `view`, turning the others off. Cleanup without rules says so and changes
/// nothing else.
pub fn switch(app: &mut App, view: View) -> Vec<Effect> {
    if view == View::Cleanup && app.snap.rules.is_none() {
        return cleanup::show(app, true);
    }
    if view != View::Largest {
        largest::show(app, false);
    }
    match view {
        View::Files => {
            dupes::show(app, false);
            cleanup::show(app, false);
            treemap::show(app, false);
            Vec::new()
        }
        View::Map => {
            dupes::show(app, false);
            cleanup::show(app, false);
            tree::act::treemap_dir(app);
            treemap::show(app, true);
            Vec::new()
        }
        View::Cleanup => {
            dupes::show(app, false);
            cleanup::show(app, true)
        }
        View::Duplicates => dupes::show(app, true),
        View::Largest => {
            largest::show(app, true);
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
        if view != View::Cleanup || app.snap.rules.is_some() {
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
            Some(n) => crate::theme::count(n as u64),
            None => "…".into(),
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
    use crate::features::treemap::tests::{classified, sync};
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
        let fx = classified();
        let t0 = Instant::now();
        let mut app = App::new(Theme::no_color(), t0);
        sync(&mut app, &fx.m, t0);
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
        assert!(app.filter.active());
        app.key(press(KeyCode::Esc), t0);
        app.reorder(t0);
        assert!(!app.filter.active());
        assert_eq!(current(&app), View::Map);
        app.key(press(KeyCode::Esc), t0);
        assert_eq!(current(&app), View::Files);
    }

    #[test]
    fn short_sizes_drop_the_space_and_the_binary_suffix() {
        assert_eq!(short_size(512), "512B");
        assert_eq!(short_size(3 * (1 << 30) + (1 << 30) / 10), "3.1G");
        assert_eq!(short_size(1 << 20), "1.0M");
    }
}

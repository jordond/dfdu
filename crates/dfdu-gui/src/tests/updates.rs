//! The update notice shows the newer release the settings know of until it is dismissed, and
//! the Updates window opens from the View menu with its switch and the command-line tool. The
//! release server is never reached: the window checks against a closed port.

use dfdu_session::startup::Source;
use egui_kittest::kittest::Queryable;

use super::{files, harness_with, local, settle, shown, shown_part, until};
use crate::launch::{self, Launch};
use crate::settings::Prefs;

fn start(dir: &std::path::Path) -> Launch {
    Launch {
        source: Source::Local {
            path: None,
            start: launch::local(None),
        },
        ..local(dir, true, None)
    }
}

/// Settings whose last check was just made, so none starts on its own.
fn checked(latest: Option<&str>) -> Prefs {
    Prefs {
        latest: latest.map(str::to_owned),
        checked: crate::update::unix_now(),
        ..Prefs::default()
    }
}

#[test]
fn a_newer_release_is_noticed_until_dismissed() {
    let tmp = files();
    let mut h = harness_with(start(tmp.path()), checked(Some("99.0.0")), None);
    until(&mut h, "the notice", |h| {
        shown(h, "dfdu 99.0.0 is available")
    });
    h.get_by_label("Not now").click();
    until(&mut h, "no notice", |h| {
        !shown(h, "dfdu 99.0.0 is available")
    });
    let prefs = h.state().gui.as_ref().expect("the window").prefs();
    assert_eq!(prefs.skipped.as_deref(), Some("99.0.0"));
}

#[test]
fn the_release_this_is_is_not_noticed() {
    let tmp = files();
    let mut h = harness_with(start(tmp.path()), checked(Some(dfdu_update::CURRENT)), None);
    until(&mut h, "the start screen", |h| shown(h, "Browse…"));
    assert!(!shown_part(&h, "is available"));
}

#[test]
fn the_updates_window_opens_from_the_view_menu() {
    let tmp = files();
    let mut h = harness_with(start(tmp.path()), checked(None), None);
    until(&mut h, "the start screen", |h| shown(h, "Browse…"));
    h.get_by_label("View").click();
    until(&mut h, "the menu", |h| {
        shown(h, "Updates and command-line tool…")
    });
    h.get_by_label("Updates and command-line tool…").click();
    until(&mut h, "the window", |h| shown(h, "Command-line tool"));
    assert!(shown_part(
        &h,
        &format!("This is dfdu {}", dfdu_update::CURRENT)
    ));
    // The closed port answers at once.
    until(&mut h, "the failed check", |h| {
        shown(h, "Could not check for updates")
    });
    assert!(shown(&h, "Try again"));
    // The modal centres itself a frame after it grows (with the failed check, then with the
    // command-line tool found): a click in that frame lands where the switch was.
    until(&mut h, "the command-line tool looked for", |h| {
        shown(h, "Not installed.") || shown_part(h, " is at ")
    });
    settle(&mut h);

    h.get_by_label("Check automatically").click();
    until(&mut h, "the switch", |h| {
        !h.state().gui.as_ref().expect("the window").prefs().updates
    });
    // The footer's Close; the header has a Close of its own.
    h.query_all_by_label("Close")
        .last()
        .expect("a Close")
        .click();
    until(&mut h, "no window", |h| !shown(h, "Command-line tool"));
}

//! The update notice shows the newer release the settings know of until it is dismissed. The
//! release server is never reached: the window checks against a closed port.

use dfdu_session::startup::Source;
use egui_kittest::kittest::Queryable;

use super::{files, harness_with, local, shown, shown_part, until};
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

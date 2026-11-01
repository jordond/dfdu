//! The banner on Full Disk Access: the window shows it when the scan could not read folders
//! macOS keeps from it, and it goes once dismissed. The folders are given to the window here,
//! as the check on macOS finds them (`access::denied`); its button, which opens System
//! Settings, is not clicked.

use egui_kittest::kittest::Queryable;

use super::{close, files, scan_done, shown, shown_part, until, window};

#[test]
fn the_banner_on_full_disk_access_names_the_folders_and_is_dismissed() {
    let tmp = files();
    let mut h = window(tmp.path());
    until(&mut h, "the scan", |h| scan_done(h) && shown(h, "big.bin"));
    assert!(!shown_part(&h, "Full Disk Access"));
    let folders = ["Mail", "Safari", "Messages", "Photos"].map(|f| format!("~/Library/{f}"));
    let gui = h.state_mut().gui.as_mut().expect("the window");
    let view = gui.browser_mut().expect("a browser").view_mut();
    view.no_access = Some(folders.to_vec());
    until(&mut h, "the banner", |h| shown(h, "No Full Disk Access."));
    assert!(shown_part(
        &h,
        "macOS keeps ~/Library/Mail, ~/Library/Safari, ~/Library/Messages and 1 more from dfdu"
    ));
    assert!(shown(&h, "Open Full Disk Access settings"));
    h.get_by_label("Dismiss").click();
    until(&mut h, "no banner", |h| !shown(h, "No Full Disk Access."));
    close(h);
}

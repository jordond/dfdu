//! Full Disk Access on macOS. A window opened from Finder does not inherit a terminal's Full
//! Disk Access: macOS privacy protection (TCC) keeps the folders of Mail, Messages, Safari,
//! Contacts, Photos and Time Machine, and Apple's app containers, from it, so their sizes are
//! missing from the scan. When a scan finishes with permission denied on one of those folders
//! ([`denied`]), a banner says why and opens the privacy settings ([`banner`], [`SETTINGS`]),
//! until it is dismissed. Elsewhere the check is not made.
//!
//! The list of folders ([`IN_HOME`], [`is_time_machine`]) is short on purpose: it names the
//! folders users hold most, and only a denial on one of them shows the banner.

use std::path::{Component, Path, PathBuf};

use dfdu_session::palette::Token;
use dfdu_tree::{ErrKind, Tree};
use egui::{Align, Button, CornerRadius, Frame, Layout, Margin, Stroke, Ui, vec2};

use crate::browser::Intent;
use crate::kit::{self, Kind};
use crate::theme::Colors;
use crate::theme::style::{CONTROL, RADIUS_MD, SPACE_2};

/// The Full Disk Access pane of System Settings, opened with `open`.
pub const SETTINGS: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles";

/// Where the data volume of macOS is mounted: a scan of it sees the home directory below it.
const DATA_VOLUME: &str = "/System/Volumes/Data";

/// The folders below the home directory that TCC keeps from an app without Full Disk Access;
/// a name ending with `*` stands for every name it starts.
pub const IN_HOME: &[&str] = &[
    "Library/Mail",
    "Library/Messages",
    "Library/Safari",
    "Library/Application Support/AddressBook",
    "Library/Application Support/CallHistoryDB",
    "Library/Containers/com.apple.*",
    "Pictures/Photos Library.photoslibrary",
];

/// The most folders the banner names.
const NAMED: usize = 3;

/// Whether `path`, or a folder above it, is one Time Machine keeps: a backup disk's
/// `Backups.backupdb`, or the local snapshots and backups macOS mounts below `/Volumes`.
pub fn is_time_machine(path: &Path) -> bool {
    let named = |c: Component<'_>| {
        c.as_os_str() == "Backups.backupdb"
            || c.as_os_str() == ".timemachine"
            || c.as_os_str() == "com.apple.TimeMachine.localsnapshots"
    };
    path.components().any(named)
}

/// Whether `path` is one of the folders TCC keeps from an app without Full Disk Access, or is
/// inside one, with the home directory `home`.
pub fn protected(path: &Path, home: Option<&Path>) -> bool {
    if is_time_machine(path) {
        return true;
    }
    let Some(home) = home else { return false };
    let path = path
        .strip_prefix(DATA_VOLUME)
        .map_or_else(|_| path.to_path_buf(), |rest| Path::new("/").join(rest));
    let Ok(rel) = path.strip_prefix(home) else {
        return false;
    };
    let names: Vec<_> = rel.components().map(Component::as_os_str).collect();
    IN_HOME.iter().any(|pattern| {
        let parts: Vec<&str> = pattern.split('/').collect();
        names.len() >= parts.len()
            && parts.iter().zip(&names).all(|(part, name)| {
                let name = name.to_string_lossy();
                match part.strip_suffix('*') {
                    Some(start) => name.starts_with(start),
                    None => name == *part,
                }
            })
    })
}

/// The folders of `tree` that could not be read for want of permission and that TCC keeps
/// ([`protected`]), in path order. Only subtrees with errors are visited.
pub fn denied(tree: &Tree, home: Option<&Path>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack: Vec<_> = tree.roots().collect();
    while let Some(ix) = stack.pop() {
        if tree.error(ix) == Some(ErrKind::PermissionDenied) {
            let path = tree.host_path(ix).to_local();
            if protected(&path, home) {
                out.push(path);
                continue;
            }
        }
        let mut children: Vec<_> = (tree.children(ix))
            .filter(|&c| tree.total(c).errors > 0)
            .collect();
        // Reverse name order on the stack pops in name order.
        children.sort_by(|&a, &b| tree.name(b).cmp(tree.name(a)));
        stack.extend(children);
    }
    out
}

/// The banner, a strip on `raised` outlined in `status.warn`: which folders could not be read
/// and why, a button that opens the privacy settings and a quiet one that dismisses it.
pub fn banner(ui: &mut Ui, colors: &Colors, folders: &[String], out: &mut Vec<Intent>) {
    let mut named = folders
        .iter()
        .take(NAMED)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if folders.len() > NAMED {
        named.push_str(&format!(" and {} more", folders.len() - NAMED));
    }
    let warn = colors.fg(Token::StatusWarn);
    Frame::new()
        .fill(colors.chrome.raised)
        .stroke(Stroke::new(1.0, warn))
        .corner_radius(CornerRadius::same(RADIUS_MD))
        .inner_margin(Margin::symmetric(12, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = SPACE_2;
                let dismiss =
                    Button::new(kit::label("Dismiss", 12.0).color(colors.fg(Token::FgDim)))
                        .frame(false)
                        .min_size(vec2(0.0, CONTROL));
                if ui.add(dismiss).clicked() {
                    out.push(Intent::DismissAccess);
                }
                let open = kit::button(
                    ui,
                    colors,
                    Kind::Secondary,
                    "Open System Settings",
                    None,
                    true,
                );
                if open.clicked() {
                    out.push(Intent::OpenAccessSettings);
                }
                ui.add_space(SPACE_2);
                let rest = Layout::left_to_right(Align::Center).with_main_wrap(true);
                ui.with_layout(rest, |ui| {
                    ui.spacing_mut().item_spacing.x = SPACE_2;
                    ui.label(kit::label("No Full Disk Access.", 12.0).color(warn));
                    ui.label(kit::sans(
                        format!(
                            "macOS keeps {named} from dfdu, so their sizes are missing. Allow \
                             dfdu in Privacy & Security › Full Disk Access, then reopen it and \
                             rescan."
                        ),
                        12.0,
                    ));
                });
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_folders_tcc_keeps_are_told_from_the_others() {
        let home = Path::new("/Users/u");
        let at = |rel: &str| rel.split('/').fold(home.to_path_buf(), |p, n| p.join(n));
        for kept in [
            "Library/Mail",
            "Library/Mail/V10",
            "Library/Safari",
            "Library/Application Support/AddressBook",
            "Library/Containers/com.apple.mail",
            "Library/Containers/com.apple.Safari/Data",
            "Pictures/Photos Library.photoslibrary",
        ] {
            assert!(protected(&at(kept), Some(home)), "{kept}");
        }
        for open in [
            "Library",
            "Library/Caches",
            "Library/Containers/com.docker.docker",
            "Library/MailDrop",
            "Pictures/Other.photoslibrary",
        ] {
            assert!(!protected(&at(open), Some(home)), "{open}");
        }
        // Another user's, or without a home directory: only Time Machine is known.
        assert!(!protected(
            &Path::new("/Users/v").join("Library").join("Mail"),
            Some(home)
        ));
        assert!(!protected(&at("Library/Mail"), None));
        let backups = Path::new("/Volumes/Backup").join("Backups.backupdb");
        assert!(protected(&backups, None));
        assert!(protected(&backups.join("mac"), Some(home)));
        // The home directory as a scan of the data volume sees it.
        let data = Path::new(DATA_VOLUME)
            .join("Users")
            .join("u")
            .join("Library");
        assert!(protected(&data.join("Messages"), Some(home)));
        assert!(!protected(&data.join("Caches"), Some(home)));
    }

    #[cfg(unix)]
    #[test]
    fn only_denials_on_the_folders_tcc_keeps_count() {
        use dfdu_tree::{Kind, Meta};

        let meta = Meta {
            kind: Kind::Dir,
            apparent: 0,
            alloc: 4096,
            mtime: 0,
            dev: 1,
            ino: 1,
            nlink: 2,
            mount: 0,
        };
        let mut t = Tree::new();
        let home = t.new_root(b"/Users/u", &meta, true);
        let lib = t.add_dir(home, b"Library", &meta);
        let mail = t.add_dir(lib, b"Mail", &meta);
        let safari = t.add_dir(lib, b"Safari", &meta);
        let caches = t.add_dir(lib, b"Caches", &meta);
        t.add_dir(lib, b"Messages", &meta);
        // Denied, kept by TCC; failing otherwise; denied, not kept: only the first counts.
        t.set_error(mail, ErrKind::PermissionDenied);
        t.set_error(safari, ErrKind::Os(5));
        t.set_error(caches, ErrKind::PermissionDenied);
        let found = denied(&t, Some(Path::new("/Users/u")));
        assert_eq!(found, [PathBuf::from("/Users/u/Library/Mail")]);
        assert!(denied(&t, None).is_empty());
    }
}

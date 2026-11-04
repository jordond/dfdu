//! A scan of a folder outside the one shown, asked for by its browser (up past its root, a
//! folder gone to, a breadcrumb above the root), opened the way the browser's own was
//! ([`Origin`]): on this machine as `dfdu PATH` opens it, as an administrator, or over ssh to
//! the same computer. The new scan is opened first and the old browser stopped only once it
//! is; when it cannot be opened, the old browser shows again with why ([`Back`]).

use std::io;
use std::path::PathBuf;

use dfdu_session::startup::StartLocal;
use dfdu_tree::HostPath;

use super::{Gui, Screen};
use crate::browser::Browse;
use crate::launch;
use crate::remote::Target;
use crate::start::Start;

/// How a browser's scan was opened, so a scan of another folder is opened the same way.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Origin {
    /// On this machine.
    Local,
    /// On this machine as an administrator.
    Admin,
    /// On another computer, connected to as `Target` says.
    Remote(Target),
    /// Read from a file: nothing outside it can be opened.
    Imported,
}

/// The screen to show again when what is being opened fails or is cancelled.
pub(super) enum Back {
    /// The start screen, with how a directory chosen on it is scanned.
    Start(Start, StartLocal),
    /// The browser that asked for a scan of another folder, stopped once the new one opens.
    Browse(Box<dyn Browse>, Origin),
}

impl Back {
    /// The browser to stop once the new one opens.
    pub(super) fn into_browser(self) -> Option<(Box<dyn Browse>, Origin)> {
        match self {
            Back::Browse(b, origin) => Some((b, origin)),
            Back::Start(..) => None,
        }
    }

    /// A connection made from here is kept among the recent ones: only one chosen on the start
    /// screen, not another folder of a computer already connected to.
    pub(super) fn remembers(&self) -> bool {
        matches!(self, Back::Start(..))
    }
}

/// How a scan of a folder asked for from a browser of `origin` is opened ([`plan`]).
#[derive(Debug, PartialEq)]
pub(super) enum Plan {
    /// On this machine, as `dfdu PATH` opens it.
    Local(PathBuf),
    /// On this machine as an administrator: the password dialog first.
    Admin(PathBuf),
    /// Over a new connection to the same computer.
    Connect(Target),
    /// Not at all: the scan was read from a file.
    FromFile,
}

/// How a scan of `path`, asked for from a browser of `origin`, is opened.
pub(super) fn plan(origin: &Origin, path: &HostPath) -> Plan {
    match origin {
        Origin::Local => Plan::Local(path.to_local()),
        Origin::Admin => Plan::Admin(path.to_local()),
        Origin::Remote(target) => Plan::Connect(Target {
            path: path.to_string(),
            ..target.clone()
        }),
        Origin::Imported => Plan::FromFile,
    }
}

/// The screen once the browser of a new scan, opened as `origin`, has started or failed to
/// (`opened`): the browser `old` it replaces is stopped, or shows again with why, or the
/// screen says why.
pub(super) fn replaced(
    opened: io::Result<Box<dyn Browse>>,
    origin: Origin,
    old: Option<(Box<dyn Browse>, Origin)>,
) -> Screen {
    match opened {
        Ok(mut b) => {
            if let Some((old, _)) = old {
                // The folder come from is selected once the new scan lists it.
                b.state_mut().came_from = Some(old.state().snap.root_path.clone());
                old.shutdown();
            }
            Screen::Browse(b, origin)
        }
        Err(e) => match old {
            Some((mut old, origin)) => {
                old.state_mut().status = Some(format!("cannot open it: {e}"));
                Screen::Browse(old, origin)
            }
            None => Screen::Failed(format!("Cannot open it: {e}")),
        },
    }
}

/// The screen after an administrator's scan could not start: the one it was asked from, with
/// why on a browser's status line, or why alone. `why` is `None` when the password dialog was
/// cancelled.
pub(super) fn not_elevated(back: Option<Back>, why: Option<String>) -> Screen {
    match (back, why) {
        (Some(Back::Browse(mut b, origin)), why) => {
            b.state_mut().status = why;
            Screen::Browse(b, origin)
        }
        (_, Some(e)) => Screen::Failed(e),
        (Some(Back::Start(start, local)), None) => Screen::Start { start, local },
        (None, None) => Screen::Closed,
    }
}

/// The screen after a connection to `target` failed (`why`; empty when it was cancelled): the
/// one it was asked from, with why.
pub(super) fn not_connected(back: Back, target: &Target, why: String) -> Screen {
    match back {
        Back::Start(mut start, local) => {
            start.failed_remote(target, why);
            Screen::Start { start, local }
        }
        Back::Browse(mut b, origin) => {
            if !why.is_empty() {
                b.state_mut().status = Some(format!("cannot scan {}: {why}", target.path));
            }
            Screen::Browse(b, origin)
        }
    }
}

impl Gui {
    /// Opens a scan of `path`, which the browser shown asked for, the way the browser's own
    /// was opened ([`plan`]) and with the settings it shows its list with. The browser is
    /// stopped once the new scan opens, and shows why when it cannot.
    pub(super) fn reopen(&mut self, path: &HostPath, ctx: &egui::Context) {
        // The list as the browser shows it now, for the new one.
        self.prefs = self.prefs();
        let Screen::Browse(mut b, origin) = std::mem::replace(&mut self.screen, Screen::Closed)
        else {
            return;
        };
        self.screen = match plan(&origin, path) {
            Plan::Local(local) => {
                // Taken before the old browser is stopped: what it read below the folder.
                let scanned = b.scanned(path);
                let source = launch::reopened(local, &self.settings);
                let opened = self.start_browser(source, scanned, ctx);
                replaced(opened, Origin::Local, Some((b, origin)))
            }
            Plan::Admin(path) => self.elevate(path, Some(Back::Browse(b, origin)), ctx),
            Plan::Connect(target) => {
                let conn = crate::remote::Connecting::start(target, &self.settings, ctx);
                Screen::Connecting {
                    conn: Box::new(conn),
                    back: Back::Browse(b, origin),
                }
            }
            Plan::FromFile => {
                b.state_mut().status = Some(crate::goto::FROM_FILE.into());
                Screen::Browse(b, origin)
            }
        };
        ctx.request_repaint();
    }
}

#[cfg(test)]
mod tests;

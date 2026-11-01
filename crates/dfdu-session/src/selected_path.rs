//! The selected path handed to the rest of the desktop: the path of what is selected in the
//! view shown ([`selected`]): the list's (and the treemap's) selected entry, the cleanup view's
//! item, the duplicate file or the largest file under the cursor. Showing it in the file
//! manager ([`open`]) is an effect a front end carries out without waiting for it
//! ([`crate::file_manager::spawn_open`]); copying it is the front end's, as the clipboard is.
//! A scan of another machine (`--remote`) or one read from a file has no file manager here to
//! show its paths in.

use dfdu_engine::tilde;
use dfdu_tree::HostPath;

use crate::effect::Effect;
use crate::state::SessionState;
use crate::view::View;
use crate::{cleanup, dupes, largest};

/// The path of what is selected in the view shown, and whether it is a directory.
pub fn selected(s: &SessionState) -> Option<(HostPath, bool)> {
    match s.current_view() {
        View::Largest => return largest::selected(s).map(|(path, _)| (path, false)),
        View::Duplicates => {
            return dupes::selected(s).map(|p| (HostPath::local(&p), false));
        }
        View::Cleanup => {
            let it = cleanup::selected(s)?;
            return Some((it.path.clone(), it.kind == dfdu_tree::Kind::Dir));
        }
        View::Files | View::Map => {}
    }
    let index = s.list.rows[s.selected_index()?];
    let e = &s.snap.entries[index];
    if e.pseudo {
        return None;
    }
    Some((crate::tree::path_of(&s.snap, index), e.is_dir))
}

/// Shows the selected entry in the file manager, unless the scan is not of this machine, and
/// says so in the status line.
pub fn open(s: &mut SessionState) -> Option<Effect> {
    if !s.outside.local {
        s.status =
            Some("o opens this machine's file manager; this scan is remote or from a file".into());
        return None;
    }
    let Some((path, is_dir)) = selected(s) else {
        s.status = Some("nothing selected to open".into());
        return None;
    };
    let shown = tilde(&path, s.home.as_deref());
    s.status = Some(format!("opening {shown} in the file manager…"));
    Some(Effect::Open {
        path: path.to_local(),
        is_dir,
    })
}

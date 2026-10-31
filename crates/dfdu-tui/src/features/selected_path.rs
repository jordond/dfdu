//! The selected path handed to the rest of the desktop: `y` copies it to the clipboard and `o`
//! shows it in the file manager. The selected path is that of the view shown: the list's (and
//! the treemap's) selected entry, the cleanup view's item, the duplicate file or the largest
//! file under the cursor.
//!
//! Both go through the run loop as effects. The copy is an OSC 52 escape written to the
//! terminal, which puts the text on the clipboard of the machine the terminal runs on, also
//! over SSH, in terminals that allow it. The file manager is started without waiting for it:
//! `open` on macOS, `explorer` on Windows and `xdg-open` elsewhere, a file shown in its folder
//! (selected, where the file manager can do that). A scan of another machine (`--remote`) or
//! one read from a file has no file manager here to show its paths in.

use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use dfdu_tree::HostPath;

use crate::app::views::{self, View};
use crate::app::{App, Effect};
use crate::features::{cleanup, dupes, largest};
use crate::model::outside::tilde;

/// The path of what is selected in the view shown, and whether it is a directory.
fn selected(app: &App) -> Option<(HostPath, bool)> {
    match views::current(app) {
        View::Largest => return largest::selected(app).map(|(path, _)| (path, false)),
        View::Duplicates => {
            return dupes::selected(app).map(|p| (HostPath::local(&p), false));
        }
        View::Cleanup => {
            let it = cleanup::selected(app)?;
            return Some((it.path.clone(), it.kind == dfdu_tree::Kind::Dir));
        }
        View::Files | View::Map => {}
    }
    let index = app.list.rows[app.selected_index()?];
    let e = &app.snap.entries[index];
    if e.pseudo {
        return None;
    }
    Some((crate::state::tree::path_of(&app.snap, index), e.is_dir))
}

/// `y`: copies the selected path, and says so.
pub fn copy(app: &mut App) -> Option<Effect> {
    let Some((path, _)) = selected(app) else {
        app.status = Some("nothing selected to copy".into());
        return None;
    };
    let text = path.to_string();
    let shown = tilde(&path, app.home.as_deref());
    app.status = Some(format!("copied {shown} (through the terminal, OSC 52)"));
    Some(Effect::Copy(text))
}

/// `o`: shows the selected entry in the file manager, unless the scan is not of this machine.
pub fn open(app: &mut App) -> Option<Effect> {
    if !app.outside.local {
        app.status =
            Some("o opens this machine's file manager; this scan is remote or from a file".into());
        return None;
    }
    let Some((path, is_dir)) = selected(app) else {
        app.status = Some("nothing selected to open".into());
        return None;
    };
    let shown = tilde(&path, app.home.as_deref());
    app.status = Some(format!("opening {shown} in the file manager…"));
    Some(Effect::Open {
        path: path.to_local(),
        is_dir,
    })
}

/// The OSC 52 escape that sets the clipboard to `text`.
pub fn osc52(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64(text.as_bytes()))
}

/// Writes [`osc52`] of `text` to `out`, the terminal.
///
/// # Errors
///
/// When writing to `out` fails.
pub fn write_osc52(out: &mut impl Write, text: &str) -> io::Result<()> {
    out.write_all(osc52(text).as_bytes())?;
    out.flush()
}

/// Standard base64 with padding, as OSC 52 takes it.
fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            if i <= chunk.len() {
                out.push(char::from(ABC[((n >> shift) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The command that shows `path` in this platform's file manager: a directory opened, a file
/// shown in its folder.
pub fn open_command(path: &Path, is_dir: bool) -> Command {
    let mut c;
    if cfg!(target_os = "macos") {
        c = Command::new("open");
        if !is_dir {
            c.arg("-R");
        }
        c.arg(path);
    } else if cfg!(windows) {
        c = Command::new("explorer");
        if is_dir {
            c.arg(path);
        } else {
            let mut select = std::ffi::OsString::from("/select,");
            select.push(path);
            c.arg(select);
        }
    } else {
        c = Command::new("xdg-open");
        c.arg(if is_dir {
            path
        } else {
            path.parent().unwrap_or(path)
        });
    }
    c
}

/// Starts the file manager on `path` without waiting for it: a thread waits for the command
/// to end, so it leaves no zombie process behind. An error goes to the status line.
pub fn spawn_open(app: &mut App, path: &Path, is_dir: bool) {
    let spawned = open_command(path, is_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match spawned {
        Ok(mut child) => {
            let waiter = std::thread::Builder::new()
                .name("dfdu-open".into())
                .spawn(move || child.wait());
            if let Err(e) = waiter {
                app.status = Some(format!("cannot wait for the file manager: {e}"));
            }
        }
        Err(e) => {
            let program = open_command(path, is_dir).get_program().to_owned();
            app.status = Some(format!(
                "cannot start {}: {e}",
                Path::new(&program).display()
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_pads_and_encodes() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"/home/jo/dev"), "L2hvbWUvam8vZGV2");
        assert_eq!(base64("é".as_bytes()), "w6k=");
    }

    #[test]
    fn osc52_is_written_whole() {
        let mut out = Vec::new();
        write_osc52(&mut out, "/r/a").expect("write");
        assert_eq!(out, b"\x1b]52;c;L3IvYQ==\x07");
    }

    #[test]
    fn file_manager_shows_a_file_in_its_folder() {
        let args = |c: &Command| -> Vec<String> {
            c.get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect()
        };
        let dir = open_command(Path::new("/r/dir"), true);
        let file = open_command(Path::new("/r/dir/a.txt"), false);
        if cfg!(target_os = "macos") {
            assert_eq!(dir.get_program(), "open");
            assert_eq!(args(&dir), ["/r/dir"]);
            assert_eq!(args(&file), ["-R", "/r/dir/a.txt"]);
        } else if cfg!(windows) {
            assert_eq!(dir.get_program(), "explorer");
            assert_eq!(args(&file), ["/select,/r/dir/a.txt"]);
        } else {
            assert_eq!(dir.get_program(), "xdg-open");
            assert_eq!(args(&dir), ["/r/dir"]);
            assert_eq!(args(&file), ["/r/dir"]);
        }
    }
}

//! The file manager of this machine, which shows a path without the front end waiting for it:
//! `open` on macOS, `explorer` on Windows and `xdg-open` elsewhere, a file shown in its folder
//! (selected, where the file manager can do that).

use std::path::Path;
use std::process::{Command, Stdio};

/// The command that shows `path` in this platform's file manager: a directory opened, a file
/// shown in its folder.
#[must_use]
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
/// to end, so it leaves no zombie process behind.
///
/// # Errors
///
/// When the file manager or the thread waiting for it cannot be started: the problem, worded
/// for the status line.
pub fn spawn_open(path: &Path, is_dir: bool) -> Result<(), String> {
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
            match waiter {
                Ok(_) => Ok(()),
                Err(e) => Err(format!("cannot wait for the file manager: {e}")),
            }
        }
        Err(e) => {
            let program = open_command(path, is_dir).get_program().to_owned();
            Err(format!(
                "cannot start {}: {e}",
                Path::new(&program).display()
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

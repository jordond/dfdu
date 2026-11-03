//! Go to a folder: a path typed in a prompt, resolved against the directory shown
//! ([`resolve`]), and the completion of its last component from the directories on the scanned
//! host ([`complete`]). A path inside the scan is opened there; one outside it is a new scan,
//! which the front end opens ([`go`]). Going up from the scan root is one too: the first
//! `Parent` there only says so, a second in a row opens the parent ([`up_at_root`]).
//!
//! Paths are resolved lexically: `.` and `..` are dropped and taken off, links are not
//! followed. `~` is the home directory on a scan of this machine.

use std::io;
use std::path::Path;

use dfdu_tree::HostPath;

use crate::actions::{Action, first_key};
use crate::state::{Moved, SessionState};

/// Where a typed path goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Dest {
    /// The scan root or a directory below it: the browser opens it, with no new scan.
    Within(HostPath),
    /// A path outside the scan: a new scan of it.
    Outside(HostPath),
}

/// What [`go`] did.
#[derive(Clone, Debug)]
pub enum Gone {
    /// The browser opened a directory of the scan; `None` when it was the one shown.
    Within(Option<Moved>),
    /// The path is outside the scan: the front end opens a new scan of it.
    Outside(HostPath),
}

/// What a completion found: the input completed as far as the names agree, and the names it
/// can still become when there are several.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Completion {
    /// The input with its last component completed; `None` when nothing could be added.
    pub text: Option<String>,
    /// The directory names that start with what was typed, sorted, when more than one does.
    pub candidates: Vec<String>,
}

/// The notice for a scan read from a file, which has nothing outside it to open.
pub const FROM_FILE: &str =
    "this scan was read from a file: run dfdu with a folder to scan another one";

/// The path `input` names, seen from `current` (the directory shown) in a scan of `root`:
/// absolute, or relative to `current`; `~` is `home` on a scan of this machine (`local`).
/// `.` and `..` are resolved lexically, and `..` stops at the root of the filesystem.
///
/// # Errors
///
/// When nothing is typed, or `~` is typed on a scan that is not of this machine.
pub fn resolve(
    input: &str,
    current: &HostPath,
    root: &HostPath,
    home: Option<&Path>,
    local: bool,
) -> Result<Dest, String> {
    let path = absolute(input, current, home, local)?;
    Ok(if path.starts_with(root) {
        Dest::Within(path)
    } else {
        Dest::Outside(path)
    })
}

/// The absolute, lexically normal path `input` names from `current`.
fn absolute(
    input: &str,
    current: &HostPath,
    home: Option<&Path>,
    local: bool,
) -> Result<HostPath, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("type the path of a folder".into());
    }
    let host = current.host();
    let typed = HostPath::new(host, input.as_bytes().to_vec());
    let tilde = input == "~" || input.starts_with("~/") || input.starts_with("~\\");
    let full = if tilde {
        let Some(home) = home.filter(|_| local) else {
            return Err("~ is known only on a scan of this computer".into());
        };
        let mut p = HostPath::local(home);
        p.push(input[1..].trim_start_matches(['/', '\\']).as_bytes());
        p
    } else if typed.is_absolute() {
        typed
    } else {
        current.join(input.as_bytes())
    };
    Ok(normal(&full))
}

/// `p` (absolute) with its `.` components dropped and each `..` taking off the one before it.
fn normal(p: &HostPath) -> HostPath {
    let mut root = p.clone();
    while let Some(up) = root.parent() {
        root = up;
    }
    let mut parts: Vec<&[u8]> = Vec::new();
    for c in p.components() {
        match c {
            b"." => {}
            b".." => {
                parts.pop();
            }
            c => parts.push(c),
        }
    }
    let mut out = root;
    for c in parts {
        out.push(c);
    }
    out
}

/// Completes the last component of `input` (seen from `current`, as [`resolve`] reads it)
/// from the directories `list` finds in the directory before it. Names starting with `.` are
/// offered only when what was typed of the last component starts with `.`.
pub fn complete(
    input: &str,
    current: &HostPath,
    home: Option<&Path>,
    local: bool,
    list: impl Fn(&HostPath) -> io::Result<Vec<String>>,
) -> Completion {
    let host = current.host();
    let cut = input
        .bytes()
        .rposition(|b| host.is_sep(b))
        .map_or(0, |i| i + 1);
    let (dir_text, typed) = input.split_at(cut);
    let dir = if dir_text.is_empty() {
        Ok(current.clone())
    } else {
        absolute(dir_text, current, home, local)
    };
    let Ok(names) = dir.and_then(|d| list(&d).map_err(|e| e.to_string())) else {
        return Completion::default();
    };
    let mut found: Vec<String> = (names.into_iter())
        .filter(|n| n.starts_with(typed) && (typed.starts_with('.') || !n.starts_with('.')))
        .collect();
    found.sort();
    let sep = char::from(host.sep());
    match found.as_slice() {
        [] => Completion::default(),
        [one] => Completion {
            text: Some(format!("{dir_text}{one}{sep}")),
            candidates: Vec::new(),
        },
        [first, rest @ ..] => {
            let mut common = first.len();
            for n in rest {
                common = common.min(shared(first, n));
            }
            let common = &first[..common];
            Completion {
                text: (common.len() > typed.len()).then(|| format!("{dir_text}{common}")),
                candidates: found,
            }
        }
    }
}

/// The length in bytes of the prefix `a` and `b` share, on a character boundary.
fn shared(a: &str, b: &str) -> usize {
    (a.char_indices())
        .zip(b.chars())
        .find(|((_, x), y)| x != y)
        .map_or(a.len().min(b.len()), |((i, _), _)| i)
}

/// The directories in `dir` on this machine, by name; names that are not UTF-8 cannot be typed
/// and are left out. Links to directories count as directories.
///
/// # Errors
///
/// When the directory cannot be read.
pub fn list_local(dir: &HostPath) -> io::Result<Vec<String>> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir.to_local())? {
        let e = e?;
        let is_dir = match e.file_type() {
            Ok(t) if t.is_symlink() => std::fs::metadata(e.path()).is_ok_and(|m| m.is_dir()),
            Ok(t) => t.is_dir(),
            Err(_) => false,
        };
        if is_dir && let Ok(name) = e.file_name().into_string() {
            out.push(name);
        }
    }
    Ok(out)
}

/// The text a go-to prompt starts with: the directory shown, ending with a separator so what
/// is typed next goes below it.
pub fn prefill(s: &SessionState) -> String {
    let dir = &s.snap.dir_path;
    let mut text = dir.display().into_owned();
    if !dir.as_bytes().last().is_some_and(|&b| dir.host().is_sep(b)) {
        text.push(char::from(dir.host().sep()));
    }
    text
}

/// [`complete`] for the browser `s`: from this machine's directories on a scan of this machine,
/// and nothing on another host's scan, whose directories cannot be listed from here.
pub fn complete_in(s: &SessionState, input: &str) -> Completion {
    if !s.outside.local {
        return Completion::default();
    }
    complete(input, &s.snap.dir_path, s.home.as_deref(), true, list_local)
}

/// Goes to the folder `input` names in the browser `s`: opens it when it is in the scan, or
/// returns it for a new scan. On a scan of this machine the path must be a directory, and a
/// path outside the scan that leads into it through links is taken as the one inside.
///
/// # Errors
///
/// What is wrong with the path, to show in the prompt: nothing typed, no such folder, not a
/// folder, or a path outside a scan read from a file.
pub fn go(s: &mut SessionState, input: &str) -> Result<Gone, String> {
    let root = s.snap.root_path.clone();
    let local = s.outside.local;
    let mut dest = resolve(input, &s.snap.dir_path, &root, s.home.as_deref(), local)?;
    if local {
        let (Dest::Within(p) | Dest::Outside(p)) = &dest;
        check_local(p)?;
        if let Dest::Outside(p) = &dest
            && let Ok(real) = dunce::canonicalize(p.to_local())
        {
            let real = HostPath::local(&real);
            if real.starts_with(&root) {
                dest = Dest::Within(real);
            }
        }
    }
    match dest {
        Dest::Within(p) => Ok(Gone::Within(s.open_dir(p))),
        Dest::Outside(_) if s.from_file => Err(FROM_FILE.into()),
        Dest::Outside(p) => Ok(Gone::Outside(p)),
    }
}

/// Whether `p` on this machine is a directory, and if not, why not.
fn check_local(p: &HostPath) -> Result<(), String> {
    match std::fs::metadata(p.to_local()) {
        Ok(m) if m.is_dir() => Ok(()),
        Ok(_) => Err(format!("not a folder: {p}")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Err(format!("no such folder: {p}")),
        Err(e) => Err(format!("{p}: {e}")),
    }
}

/// `Parent` at the scan root: the first press says what a second one does, and a second in a
/// row (`armed`) returns the parent to scan. Nothing above the root of the filesystem, and
/// nothing outside a scan read from a file.
pub fn up_at_root(s: &mut SessionState, armed: bool) -> Option<HostPath> {
    if s.from_file {
        s.status = Some(FROM_FILE.into());
        return None;
    }
    let Some(parent) = s.snap.root_path.parent() else {
        s.status = Some("already at the top of the filesystem".into());
        return None;
    };
    if armed {
        return Some(parent);
    }
    s.up_armed = true;
    s.status = Some(format!(
        "top of the scan: press {} again to scan {parent}",
        first_key(Action::Parent)
    ));
    None
}

#[cfg(test)]
mod tests;

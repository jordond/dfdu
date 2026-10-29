//! Paths on the host a tree was scanned on, which need not be this one: a remote agent's or an
//! imported scan's. A [`HostPath`] keeps the bytes the tree stores (see [`crate::name_bytes`])
//! with that host's separators, so a Unix host's names that are not UTF-8 and its names
//! holding `\` survive a Windows client, and a Windows host's paths keep their form on a Unix
//! one. They become a `PathBuf` only where they are handed to this machine's OS
//! ([`HostPath::to_local`]).

use std::borrow::Cow;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::{name_bytes, name_os};

/// The kind of system a path names a file on, which decides how it is separated.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub enum Host {
    Unix,
    Windows,
}

impl Host {
    /// This machine.
    pub const LOCAL: Host = if cfg!(windows) {
        Host::Windows
    } else {
        Host::Unix
    };

    /// The separator paths on the host are joined with.
    #[must_use]
    pub fn sep(self) -> u8 {
        match self {
            Host::Unix => b'/',
            Host::Windows => b'\\',
        }
    }

    /// Whether the host reads `b` as a separator: Windows takes `/` as well.
    #[must_use]
    pub fn is_sep(self, b: u8) -> bool {
        b == b'/' || (self == Host::Windows && b == b'\\')
    }

    /// The host a root path looks like it is from, for a scan read from a file, which does not
    /// say: a drive letter or a `\\` prefix is Windows, a leading `/` is Unix, anything else is
    /// taken to be this machine's.
    #[must_use]
    pub fn guess(root: &[u8]) -> Host {
        match root {
            [b'/', ..] => Host::Unix,
            [b'\\', b'\\', ..] | [b'A'..=b'Z' | b'a'..=b'z', b':', ..] => Host::Windows,
            _ => Host::LOCAL,
        }
    }

    /// Bytes for the wire.
    #[must_use]
    pub fn to_u8(self) -> u8 {
        match self {
            Host::Unix => 0,
            Host::Windows => 1,
        }
    }

    #[must_use]
    pub fn from_u8(b: u8) -> Option<Host> {
        match b {
            0 => Some(Host::Unix),
            1 => Some(Host::Windows),
            _ => None,
        }
    }
}

impl Default for Host {
    fn default() -> Host {
        Host::LOCAL
    }
}

/// A path on `host`, as the bytes a tree stores names in. Paths compare and hash byte for byte,
/// so one compared with a tree's comes from that tree ([`crate::Tree::host_path`], [`HostPath::join`]):
/// on Windows, `C:/a` and the tree's `C:\a` are two.
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct HostPath {
    host: Host,
    bytes: Vec<u8>,
}

impl HostPath {
    #[must_use]
    pub fn new(host: Host, bytes: Vec<u8>) -> HostPath {
        HostPath { host, bytes }
    }

    /// The empty path on `host`: a relative path with no components.
    #[must_use]
    pub fn empty(host: Host) -> HostPath {
        HostPath::new(host, Vec::new())
    }

    /// A path on this machine.
    #[must_use]
    pub fn local(p: &Path) -> HostPath {
        HostPath::new(Host::LOCAL, name_bytes(p.as_os_str()).into_owned())
    }

    #[must_use]
    pub fn host(&self) -> Host {
        self.host
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// The path for this machine's OS. Only for a path on this machine: another host's path
    /// names nothing here.
    #[must_use]
    pub fn to_local(&self) -> PathBuf {
        debug_assert_eq!(self.host, Host::LOCAL, "{self}");
        PathBuf::from(name_os(&self.bytes).into_owned())
    }

    /// Appends `name`, with a separator unless the path is empty or ends with one.
    pub fn push(&mut self, name: &[u8]) {
        if self.bytes.last().is_some_and(|&b| !self.host.is_sep(b)) {
            self.bytes.push(self.host.sep());
        }
        self.bytes.extend_from_slice(name);
    }

    #[must_use]
    pub fn join(&self, name: &[u8]) -> HostPath {
        let mut out = self.clone();
        out.push(name);
        out
    }

    /// The components of the path below `base`, when it is `base` or below it; empty for
    /// `base` itself. Byte for byte: both are expected to come from the same tree.
    #[must_use]
    pub fn rel(&self, base: &HostPath) -> Option<Vec<&[u8]>> {
        let rest = self.bytes.strip_prefix(base.bytes.as_slice())?;
        let at_sep = base.bytes.last().is_some_and(|&b| self.host.is_sep(b));
        if !rest.is_empty() && !at_sep && !self.host.is_sep(rest[0]) {
            return None;
        }
        Some(self.split(rest).collect())
    }

    #[must_use]
    pub fn starts_with(&self, base: &HostPath) -> bool {
        self.rel(base).is_some()
    }

    /// The components after the root prefix (`/`, `C:\`, `\\server\share\`).
    pub fn components(&self) -> impl Iterator<Item = &[u8]> {
        self.split(&self.bytes[self.root_len()..])
    }

    fn split<'a>(&self, b: &'a [u8]) -> impl Iterator<Item = &'a [u8]> + use<'a> {
        let host = self.host;
        b.split(move |&c| host.is_sep(c)).filter(|c| !c.is_empty())
    }

    /// The last component, unless the path is only a root prefix or empty.
    #[must_use]
    pub fn file_name(&self) -> Option<&[u8]> {
        self.components().last()
    }

    /// The path without its last component; `None` for a root prefix or a single relative
    /// component.
    #[must_use]
    pub fn parent(&self) -> Option<HostPath> {
        let root = self.root_len();
        let mut end = self.bytes.len();
        while end > root && self.host.is_sep(self.bytes[end - 1]) {
            end -= 1;
        }
        if end <= root {
            return None;
        }
        let cut = self.bytes[root..end]
            .iter()
            .rposition(|&c| self.host.is_sep(c))
            .map(|i| root + i);
        let mut keep = match cut {
            Some(i) => i,
            None if root > 0 => root,
            None => return None,
        };
        while keep > root && self.host.is_sep(self.bytes[keep - 1]) {
            keep -= 1;
        }
        Some(HostPath::new(self.host, self.bytes[..keep].to_vec()))
    }

    /// Whether the path starts at a root: `/` on Unix; a drive and separator or a `\\` prefix
    /// on Windows.
    #[must_use]
    pub fn is_absolute(&self) -> bool {
        match self.host {
            Host::Unix => self.bytes.first() == Some(&b'/'),
            Host::Windows => {
                let b = &self.bytes;
                (b.len() >= 2 && self.host.is_sep(b[0]) && self.host.is_sep(b[1]))
                    || (b.len() >= 3
                        && b[0].is_ascii_alphabetic()
                        && b[1] == b':'
                        && self.host.is_sep(b[2]))
            }
        }
    }

    /// Length of the root prefix: `/`; `C:\`; `\\server\share\` and the `\\?\` forms of both.
    fn root_len(&self) -> usize {
        let b = &self.bytes;
        let sep = |i: usize| b.get(i).is_some_and(|&c| self.host.is_sep(c));
        match self.host {
            Host::Unix => usize::from(sep(0)),
            Host::Windows => {
                let drive = |i: usize| {
                    b.get(i).is_some_and(u8::is_ascii_alphabetic) && b.get(i + 1) == Some(&b':')
                };
                // Past `n` components after `start`, each ended by a separator or the end.
                let past = |start: usize, n: usize| {
                    let mut i = start;
                    for _ in 0..n {
                        while i < b.len() && !sep(i) {
                            i += 1;
                        }
                        i = (i + 1).min(b.len());
                    }
                    i
                };
                if sep(0) && sep(1) {
                    if b.get(2).is_some_and(|&c| c == b'?' || c == b'.') && sep(3) {
                        if drive(4) {
                            return if sep(6) { 7 } else { 6 };
                        }
                        if b[4..].len() >= 3 && b[4..7].eq_ignore_ascii_case(b"UNC") && sep(7) {
                            return past(8, 2);
                        }
                        return past(4, 1);
                    }
                    return past(2, 2);
                }
                if drive(0) {
                    return if sep(2) { 3 } else { 2 };
                }
                usize::from(sep(0))
            }
        }
    }

    /// The path as text, with what is not UTF-8 shown as U+FFFD.
    #[must_use]
    pub fn display(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.bytes)
    }
}

impl fmt::Display for HostPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.display())
    }
}

impl From<&Path> for HostPath {
    fn from(p: &Path) -> HostPath {
        HostPath::local(p)
    }
}

impl From<PathBuf> for HostPath {
    fn from(p: PathBuf) -> HostPath {
        HostPath::local(&p)
    }
}

impl From<&str> for HostPath {
    fn from(p: &str) -> HostPath {
        HostPath::local(Path::new(p))
    }
}

impl From<&PathBuf> for HostPath {
    fn from(p: &PathBuf) -> HostPath {
        HostPath::local(p)
    }
}

#[cfg(test)]
mod tests;

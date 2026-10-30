//! What both front ends say about what the scan did not enter, without looking anything up:
//! the mounts it listed but stayed out of (`-x`) and the footer lines naming them, the notes on
//! rule locations behind a link that leads out of the scan ([`dfdu_rules::linked_outside`]),
//! and paths below the home directory written with `~`. The browser and the command line look
//! the mounts and links up themselves and pass in what they found.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use dfdu_rules::{Linked, RuleSet};
use dfdu_sys::MountInfo;
use dfdu_tree::{DirState, Host, HostPath, Ix, NodeFlags, Tree};

use crate::sizes::human;

/// Most mounts a footer line names, and most links out of the scan the command line names;
/// the rest are counted.
pub const LIST_MAX: usize = 5;

/// The type and used space of a mount, for a note: `zfs, 1.2 TiB used`, or `nfs4` for a
/// network filesystem, which is not asked.
#[must_use]
pub fn describe(info: &MountInfo) -> String {
    match info.used {
        Some(u) => format!("{}, {} used", info.fstype, human(u)),
        None => info.fstype.clone(),
    }
}

/// `path` with the home directory shown as `~`. `home` is this machine's: a path on another
/// host is shown as it is, and callers pass no `home` for a scan of another machine.
#[must_use]
pub fn tilde(path: &HostPath, home: Option<&Path>) -> String {
    // A home directory of `/` would turn every path into `~/...`.
    let home = home.filter(|h| h.parent().is_some() && path.host() == Host::LOCAL);
    let home = home.map(HostPath::local);
    match home.as_ref().and_then(|h| rel_text(path, h)) {
        Some(rel) if rel.is_empty() => "~".into(),
        Some(rel) => format!("~/{rel}"),
        None => path.to_string(),
    }
}

/// `path` below `base` as text, with the host's separators: empty for `base` itself, `None`
/// when it is not below it.
#[must_use]
pub fn rel_text(path: &HostPath, base: &HostPath) -> Option<String> {
    let sep = [path.host().sep()];
    let rel = path.rel(base)?;
    Some(String::from_utf8_lossy(&rel.join(&sep[..])).into_owned())
}

/// The user's home directory with symbolic links resolved, as the scan's paths are.
#[must_use]
pub fn home_dir() -> Option<PathBuf> {
    let home = etcetera::home_dir().ok()?;
    Some(dunce::canonicalize(&home).unwrap_or(home))
}

/// `~/.gradle is a link to /mnt/data/gradle (Gradle caches and wrapper distributions), not
/// scanned: dfdu report /mnt/data/gradle`, with `command` before the target.
#[must_use]
pub fn linked_note(rules: &RuleSet, l: &Linked, home: Option<&Path>, command: &str) -> String {
    let mut names: Vec<&str> = l
        .rules
        .iter()
        .map(|&r| rules.rule(r).name.as_str())
        .collect();
    names.dedup();
    let what = match names.len() {
        0..=2 => names.join(", "),
        n => format!("{}, {} and {} more", names[0], names[1], n - 2),
    };
    format!(
        "{} is a link to {} ({what}), not scanned: {command} {}",
        tilde(&HostPath::local(&l.link), home),
        l.target.display(),
        l.target.display()
    )
}

/// `and 2 more links out of the scan`, after the first `shown` of `linked` links were named;
/// `None` when all of them were.
#[must_use]
pub fn more_links(linked: usize, shown: usize) -> Option<String> {
    let n = linked.checked_sub(shown).filter(|&n| n > 0)?;
    Some(format!(
        "and {n} more link{} out of the scan",
        if n == 1 { "" } else { "s" }
    ))
}

/// A mount the scan listed but did not enter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotEntered {
    pub path: HostPath,
    /// A network filesystem, entered with `--network`; the others with `--cross-mounts`.
    pub network: bool,
}

impl NotEntered {
    /// The option that enters it.
    #[must_use]
    pub fn option(&self) -> &'static str {
        if self.network {
            "--network"
        } else {
            "--cross-mounts"
        }
    }
}

/// The mounts below `root` the scan did not enter, in path order: not kernel filesystems,
/// which are never scanned, nor second mounts of something counted elsewhere.
#[must_use]
pub fn not_entered(tree: &Tree, root: Ix) -> Vec<NotEntered> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(ix) = stack.pop() {
        let Some(d) = tree.dir(ix) else { continue };
        let f = tree.flags(ix);
        if d.state != DirState::Skipped {
            stack.extend(tree.children(ix));
        } else if f.has(NodeFlags::MOUNT_ROOT)
            && !f.has(NodeFlags::KERNEL)
            && !f.has(NodeFlags::DUP_MOUNT)
        {
            out.push(NotEntered {
                path: tree.host_path(ix),
                network: f.has(NodeFlags::NETWORK),
            });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// The footer lines for `list` (with what was looked up about each): one per option that
/// would enter them, largest first, at most [`LIST_MAX`] named.
/// `not entered: /mnt/data (zfs, 1.2 TiB used); --cross-mounts to include`.
#[must_use]
pub fn not_entered_lines(
    list: &[(NotEntered, Option<MountInfo>)],
    home: Option<&Path>,
) -> Vec<String> {
    let mut out = Vec::new();
    for network in [false, true] {
        let mut of: Vec<&(NotEntered, Option<MountInfo>)> =
            list.iter().filter(|(n, _)| n.network == network).collect();
        let Some(option) = of.first().map(|(n, _)| n.option()) else {
            continue;
        };
        of.sort_by_key(|(n, i)| {
            let used = i.as_ref().and_then(|i| i.used);
            (std::cmp::Reverse(used), n.path.clone())
        });
        let named: Vec<String> = of
            .iter()
            .take(LIST_MAX)
            .map(|(n, i)| match i {
                Some(i) => format!("{} ({})", tilde(&n.path, home), describe(i)),
                None => tilde(&n.path, home),
            })
            .collect();
        let mut line = format!("not entered: {}", named.join(", "));
        if of.len() > LIST_MAX {
            let _ = write!(line, " and {} more", of.len() - LIST_MAX);
        }
        let _ = write!(line, "; {option} to include");
        out.push(line);
    }
    out
}

#[cfg(test)]
mod tests;

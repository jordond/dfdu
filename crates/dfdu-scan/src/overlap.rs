//! Which mounts inside a scan show data the scan already reaches through another mount: bind
//! mounts, a second mount of a btrfs subvolume, mount loops.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use dfdu_sys::MountEntry;

/// Mount ids of mounts below `scope` whose contents are also reachable inside `scope` through
/// another mount of the same filesystem. Of two mounts showing the same directory, the one
/// whose source is larger (a shorter `root`) is kept, then the one with the lower mount point,
/// so the answer does not depend on scan order.
#[must_use]
pub fn duplicate_mounts(table: &[MountEntry], scope: &Path) -> HashSet<u64> {
    let mut dups = HashSet::new();
    for m in table {
        if m.mount_point == scope || !m.mount_point.starts_with(scope) {
            continue;
        }
        let seen_elsewhere = table.iter().any(|n| {
            if n.id == m.id || n.dev != m.dev {
                return false;
            }
            let Ok(rel) = m.root.strip_prefix(&n.root) else {
                return false;
            };
            let wins = n.root.as_os_str().len() < m.root.as_os_str().len()
                || (n.root == m.root && n.mount_point < m.mount_point);
            if !wins {
                return false;
            }
            // Where `m`'s directory appears through `n`; it must be inside the scan and not
            // hidden by another mount (`m` itself included).
            let at: PathBuf = if rel.as_os_str().is_empty() {
                n.mount_point.clone()
            } else {
                n.mount_point.join(rel)
            };
            at.starts_with(scope) && covering(table, &at).is_some_and(|c| c.id == n.id)
        });
        if seen_elsewhere {
            dups.insert(m.id);
        }
    }
    dups
}

/// The mount `path` resolves through: the deepest mount point containing it, the last mounted
/// of several at the same point.
fn covering<'a>(table: &'a [MountEntry], path: &Path) -> Option<&'a MountEntry> {
    table
        .iter()
        .enumerate()
        .filter(|(_, e)| path.starts_with(&e.mount_point))
        .max_by_key(|(i, e)| (e.mount_point.components().count(), *i))
        .map(|(_, e)| e)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(id: u64, dev: u32, root: &str, mp: &str) -> MountEntry {
        MountEntry {
            id,
            parent: 0,
            dev: (0, dev),
            root: root.into(),
            mount_point: mp.into(),
            fstype: "btrfs".into(),
            source: String::new(),
        }
    }

    fn dups(table: &[MountEntry], scope: &str) -> Vec<u64> {
        let mut v: Vec<u64> = duplicate_mounts(table, Path::new(scope))
            .into_iter()
            .collect();
        v.sort_unstable();
        v
    }

    #[test]
    fn second_mount_of_a_subvolume() {
        let t = [
            m(1, 9, "/", "/"),
            m(50, 41, "/", "/f"),
            m(51, 41, "/sub", "/f/mnt"),
        ];
        assert_eq!(dups(&t, "/f"), [51]);
        // Scanning only the second mount: the first is outside the scan.
        assert_eq!(dups(&t, "/f/mnt"), Vec::<u64>::new());
        // Scanning from the top, the subvolume is reached through mount 50.
        assert_eq!(dups(&t, "/"), [51]);
    }

    #[test]
    fn two_binds_of_an_outside_directory() {
        let t = [
            m(1, 9, "/", "/"),
            m(2, 9, "/x", "/s/b"),
            m(3, 9, "/x", "/s/a"),
        ];
        // /x itself is outside /s: /s/a (lower mount point) is kept.
        assert_eq!(dups(&t, "/s"), [2]);
        // From /, /x is reached through the root mount: both binds are duplicates.
        assert_eq!(dups(&t, "/"), [2, 3]);
    }

    #[test]
    fn bind_loop() {
        let t = [m(1, 9, "/", "/"), m(2, 9, "/home", "/home/loop")];
        assert_eq!(dups(&t, "/home"), [2]);
    }

    #[test]
    fn hidden_source_is_not_a_duplicate() {
        // /f/sub is covered by an unrelated filesystem, so mount 51 is the only way to sub.
        let t = [
            m(50, 41, "/", "/f"),
            m(60, 7, "/", "/f/sub"),
            m(51, 41, "/sub", "/f/mnt"),
        ];
        assert_eq!(dups(&t, "/f"), Vec::<u64>::new());
    }

    #[test]
    fn other_filesystems_are_independent() {
        let t = [
            m(1, 9, "/", "/"),
            m(2, 8, "/", "/mnt/usb"),
            m(3, 7, "/", "/mnt/b"),
        ];
        assert_eq!(dups(&t, "/"), Vec::<u64>::new());
    }
}

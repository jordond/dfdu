//! The disk list on Linux, from the mount table: one entry per filesystem, by device, named
//! by udev's `/dev/disk/by-label` links.

use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::io;
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

use super::Disk;
#[cfg(target_os = "linux")]
use super::{Space, gather, on_thread};
use crate::{FsClass, MountEntry};

/// The mounted filesystems, sorted by mount point. Each filesystem gets `timeout` to report
/// its size; those that do not answer are listed without one.
///
/// # Errors
///
/// When the mount table cannot be read.
#[cfg(target_os = "linux")]
pub fn disks(timeout: Duration) -> io::Result<Vec<Disk>> {
    let table = crate::mount_table()?;
    let deadline = Instant::now() + timeout;
    let mut disks = filesystems(&table);
    name(&mut disks, &labels(Path::new("/dev/disk/by-label")));
    let pending = disks
        .into_iter()
        .map(|disk| {
            let mp = disk.mount_point.clone();
            (disk, on_thread(move || statvfs_space(&mp)))
        })
        .collect();
    Ok(gather(pending, deadline))
}

/// The filesystems of the mount table `table`, without their sizes, by device: one entry per
/// filesystem, at the mount of its top directory if there is one, else at its shortest mount
/// point, with its other mount points in `also_at`.
/// Left out: kernel filesystems, mounts below them (`/sys/fs/cgroup`, `/dev/shm`), which a
/// scan never reaches, and mounts hidden by a later mount at the same place.
fn filesystems(table: &[MountEntry]) -> Vec<Disk> {
    use std::collections::BTreeMap;
    let kernel: Vec<&Path> = table
        .iter()
        .filter(|e| crate::classify_fs(&e.fstype) == FsClass::Kernel)
        .map(|e| e.mount_point.as_path())
        .filter(|p| p.parent().is_some())
        .collect();
    let mut by_dev: BTreeMap<u64, Vec<&MountEntry>> = BTreeMap::new();
    for (i, e) in table.iter().enumerate() {
        let hidden = table[i + 1..]
            .iter()
            .any(|later| later.mount_point == e.mount_point);
        let below_kernel = kernel.iter().any(|k| e.mount_point.starts_with(k));
        if !hidden && !below_kernel {
            by_dev.entry(e.st_dev()).or_default().push(e);
        }
    }
    let mut out = vec![];
    for (_, mut mounts) in by_dev {
        mounts.sort_by_key(|e| {
            (
                e.root.as_os_str() != "/",
                e.mount_point.as_os_str().len(),
                e.mount_point.clone(),
            )
        });
        mounts.dedup_by(|a, b| a.mount_point == b.mount_point);
        let first = mounts[0];
        out.push(Disk {
            mount_point: first.mount_point.clone(),
            also_at: mounts[1..].iter().map(|e| e.mount_point.clone()).collect(),
            fstype: first.fstype.clone(),
            source: first.source.clone(),
            label: None,
            class: crate::classify_fs(&first.fstype),
            space: None,
            role: None,
            shares_space_with: vec![],
        });
    }
    out
}

/// The labels of udev's by-label links in `dir` (`/dev/disk/by-label`), by the device each
/// leads to. None when the directory is missing or cannot be read.
fn labels(dir: &Path) -> HashMap<PathBuf, String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return HashMap::new();
    };
    entries
        .flatten()
        .filter_map(|e| {
            let dev = std::fs::canonicalize(e.path()).ok()?;
            Some((dev, unescape(&e.file_name().to_string_lossy())))
        })
        .collect()
}

/// Sets the label of each of `disks` whose source is a device in `labels`.
#[cfg(unix)]
fn name(disks: &mut [Disk], labels: &HashMap<PathBuf, String>) {
    if labels.is_empty() {
        return;
    }
    for d in disks {
        let source = Path::new(&d.source);
        if !source.is_absolute() {
            continue;
        }
        let dev = std::fs::canonicalize(source).unwrap_or_else(|_| source.to_path_buf());
        d.label = labels.get(&dev).cloned();
    }
}

/// A label as udev writes it in a link's name, with `\xNN` for the bytes a name cannot hold
/// (`My\x20Disk` is `My Disk`, `\x2f` a `/`).
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let byte = (b[i] == b'\\' && b.get(i + 1) == Some(&b'x'))
            .then(|| b.get(i + 2..i + 4))
            .flatten()
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        if let Some(byte) = byte {
            out.push(byte);
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The size of the filesystem `path` is on, as `statvfs` reports it.
#[cfg(target_os = "linux")]
pub(crate) fn statvfs_space(path: &Path) -> io::Result<Space> {
    let s = rustix::fs::statvfs(path).map_err(io::Error::from)?;
    let unit = if s.f_frsize > 0 {
        s.f_frsize
    } else {
        s.f_bsize
    };
    Ok(Space {
        total: s.f_blocks.saturating_mul(unit),
        used: s.f_blocks.saturating_sub(s.f_bfree).saturating_mul(unit),
        available: s.f_bavail.saturating_mul(unit),
        purgeable: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mount of device `dev` (`major:minor` `8:dev`) showing its directory `root` at `at`.
    fn mount(dev: u32, root: &str, at: &str, fstype: &str) -> MountEntry {
        MountEntry {
            id: 0,
            parent: 0,
            dev: (8, dev),
            root: root.into(),
            mount_point: at.into(),
            fstype: fstype.into(),
            source: format!("/dev/sd{dev}"),
        }
    }

    fn at(disk: &Disk) -> (&Path, Vec<&Path>) {
        let also: Vec<&Path> = disk.also_at.iter().map(PathBuf::as_path).collect();
        (disk.mount_point.as_path(), also)
    }

    /// A filesystem mounted at several places is listed once, at the mount of its top
    /// directory, its subvolumes and bind mounts in `also_at`.
    #[test]
    fn a_filesystem_is_listed_once_at_its_top_directory() {
        let table = [
            mount(1, "/@home", "/home", "btrfs"),
            mount(1, "/", "/mnt/pool", "btrfs"),
            mount(1, "/@", "/", "btrfs"),
            mount(2, "/", "/data", "ext4"),
        ];
        let disks = filesystems(&table);
        let places: Vec<_> = disks.iter().map(at).collect();
        let pool: (&Path, Vec<&Path>) =
            ("/mnt/pool".as_ref(), vec!["/".as_ref(), "/home".as_ref()]);
        assert_eq!(places, [pool, ("/data".as_ref(), vec![])]);
        let pool = &disks[0];
        assert_eq!(
            (pool.fstype.as_str(), pool.source.as_str()),
            ("btrfs", "/dev/sd1")
        );
        assert_eq!(pool.class, FsClass::Local);
        assert!(disks.iter().all(|d| d.space.is_none()));
    }

    /// Without a mount of its top directory, a filesystem is listed at its shortest mount
    /// point.
    #[test]
    fn without_its_top_directory_the_shortest_mount_point_is_listed() {
        let table = [
            mount(1, "/a", "/srv/long/a", "ext4"),
            mount(1, "/b", "/b", "ext4"),
        ];
        let disks = filesystems(&table);
        let places: Vec<_> = disks.iter().map(at).collect();
        let b: (&Path, Vec<&Path>) = ("/b".as_ref(), vec!["/srv/long/a".as_ref()]);
        assert_eq!(places, [b]);
    }

    /// Kernel filesystems and mounts below them are left out, and so is a mount hidden by a
    /// later one at the same place.
    #[test]
    fn kernel_and_hidden_mounts_are_left_out() {
        let table = [
            mount(1, "/", "/", "ext4"),
            mount(2, "/", "/proc", "proc"),
            mount(3, "/", "/sys", "sysfs"),
            mount(4, "/", "/sys/fs/cgroup", "tmpfs"),
            mount(5, "/", "/mnt", "ext4"),
            mount(6, "/", "/mnt", "xfs"),
        ];
        let disks = filesystems(&table);
        let shown: Vec<_> = disks
            .iter()
            .map(|d| (d.mount_point.as_path(), d.fstype.as_str()))
            .collect();
        assert_eq!(
            shown,
            [(Path::new("/"), "ext4"), (Path::new("/mnt"), "xfs")]
        );
    }

    /// udev's `\xNN` escapes give back the bytes they stand for; anything else stays as is.
    #[test]
    fn labels_are_unescaped() {
        assert_eq!(unescape(r"My\x20Disk"), "My Disk");
        assert_eq!(unescape(r"a\x2fb"), "a/b");
        assert_eq!(unescape(r"caf\xc3\xa9"), "café");
        assert_eq!(unescape(r"odd\x2"), r"odd\x2");
        assert_eq!(unescape(r"odd\xzz\"), r"odd\xzz\");
        assert_eq!(unescape("plain"), "plain");
    }

    /// Each by-label link names the device it leads to, and so the disk mounted from it; a
    /// disk from a device without a link, or from no device, has no label.
    #[cfg(unix)]
    #[test]
    fn disks_are_named_by_their_devices_links() {
        let tmp = tempfile::tempdir().unwrap();
        let dev = tmp.path().join("dev");
        let by_label = dev.join("disk/by-label");
        std::fs::create_dir_all(&by_label).unwrap();
        for d in ["sda1", "sda2", "sdb1"] {
            std::fs::write(dev.join(d), "").unwrap();
        }
        std::os::unix::fs::symlink("../../sda1", by_label.join(r"My\x20Disk")).unwrap();
        std::os::unix::fs::symlink(dev.join("sda2"), by_label.join("data")).unwrap();
        std::os::unix::fs::symlink("../../gone", by_label.join("dangling")).unwrap();
        let labels = labels(&by_label);
        assert_eq!(labels.len(), 2, "{labels:?}");

        let disk = |source: String| Disk {
            mount_point: "/".into(),
            also_at: vec![],
            fstype: "ext4".into(),
            source,
            label: None,
            class: FsClass::Local,
            space: None,
            role: None,
            shares_space_with: vec![],
        };
        let at = |d: &str| dev.join(d).to_string_lossy().into_owned();
        let mut disks = [
            disk(at("sda1")),
            disk(at("sda2")),
            disk(at("sdb1")),
            disk("server:/export".into()),
        ];
        name(&mut disks, &labels);
        let got: Vec<Option<&str>> = disks.iter().map(|d| d.label.as_deref()).collect();
        assert_eq!(got, [Some("My Disk"), Some("data"), None, None]);
    }

    /// Without the by-label directory there are no labels.
    #[test]
    fn a_missing_label_directory_has_no_labels() {
        assert!(labels(Path::new("/nonexistent/dfdu-by-label")).is_empty());
    }
}

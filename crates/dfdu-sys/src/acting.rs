//! Acting for the user who started dfdu as root through `sudo` or `pkexec`.
//!
//! A root dfdu scans as root, but everything that belongs to a person stays that person's:
//! configuration, rules, themes, the state directory (deletion journals, staging, history),
//! the scan cache, the home trash, and native commands, which run as them (`brew` refuses
//! root; `npm` or `cargo` run as root would leave root-owned files in their home).
//!
//! [`adopt`] is called first thing in `main`. When the process runs as root for another user
//! ([`detect`]: `PKEXEC_UID`, else `SUDO_UID`), it makes `HOME`, `USER` and `LOGNAME` theirs by
//! executing itself again with them set when they differ (Linux sudo resets `HOME` to root's;
//! macOS keeps it), so every lookup of the user's directories, here and in the `trash` crate,
//! finds theirs without the process changing its own environment. Files dfdu creates as root
//! in those directories are given to the user by [`give_back`]; [`run_as`] makes a command run
//! as them.
//!
//! A root process with neither variable (a root login, `sudo -i` from root) acts for no one.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The user dfdu acts for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct User {
    pub uid: u32,
    pub gid: u32,
    pub name: OsString,
    pub home: PathBuf,
    /// Supplementary groups, from the group database.
    pub groups: Vec<u32>,
}

impl User {
    /// The environment that names this user: what [`adopt`] makes the process's own and
    /// [`run_as`] gives the commands it runs. The one list of it; the rest of the user's
    /// directories (`XDG_*`, when unset) follow from `HOME`.
    #[must_use]
    pub fn env(&self) -> [(&'static str, &std::ffi::OsStr); 3] {
        [
            ("HOME", self.home.as_os_str()),
            ("USER", &self.name),
            ("LOGNAME", &self.name),
        ]
    }
}

static CURRENT: OnceLock<User> = OnceLock::new();

/// The user this process acts for, once [`adopt`] found one.
#[must_use]
pub fn current() -> Option<&'static User> {
    CURRENT.get()
}

/// The uid that files dfdu writes for its user should have: the acting user's, else this
/// process's.
#[cfg(unix)]
#[must_use]
pub fn owner_uid() -> u32 {
    current().map_or_else(|| rustix::process::getuid().as_raw(), |u| u.uid)
}

/// The user a root process runs for: the uid in `PKEXEC_UID`, else `SUDO_UID`, when it is not
/// root's and the password database knows it. `None` when not root.
#[cfg(unix)]
#[must_use]
pub fn detect() -> Option<User> {
    if !rustix::process::geteuid().is_root() {
        return None;
    }
    let uid = ["PKEXEC_UID", "SUDO_UID"]
        .iter()
        .find_map(|v| std::env::var(v).ok()?.trim().parse::<u32>().ok())?;
    if uid == 0 {
        return None;
    }
    unix::lookup(uid)
}

#[cfg(not(unix))]
#[must_use]
pub fn detect() -> Option<User> {
    None
}

/// What [`adopt`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Adopted {
    /// Not root for someone else: nothing changed.
    Nobody,
    /// Acting for this user from now on.
    User(&'static User),
}

/// Starts acting for [`detect`]'s user, if any. When `HOME`, `USER` or `LOGNAME` are not
/// theirs, executes this program again with them set (same arguments, standard streams and
/// everything else) and returns only if that fails. Call it before anything reads the
/// environment or starts a thread.
///
/// # Errors
///
/// When the program cannot be executed again.
#[cfg(unix)]
pub fn adopt() -> std::io::Result<Adopted> {
    let Some(user) = detect() else {
        return Ok(Adopted::Nobody);
    };
    if user
        .env()
        .iter()
        .any(|&(k, v)| std::env::var_os(k).as_deref() != Some(v))
    {
        use std::os::unix::process::CommandExt;
        let exe = std::env::current_exe()?;
        let mut args = std::env::args_os();
        let arg0 = args.next().unwrap_or_else(|| exe.clone().into_os_string());
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg0(arg0).args(args).envs(user.env());
        return Err(cmd.exec());
    }
    Ok(Adopted::User(CURRENT.get_or_init(|| user)))
}

#[cfg(not(unix))]
pub fn adopt() -> std::io::Result<Adopted> {
    Ok(Adopted::Nobody)
}

/// Gives what root made in `dir` back to the user dfdu acts for: `dir` and every directory
/// between their home and it, and with `deep` the directories and singly linked regular files
/// below `dir`, whenever owned by root. Symlinks are never followed, other filesystems not
/// entered, and `dir` must be below their home, without `..`. Safe against the user changing
/// the tree meanwhile (see the walk's comment). Does nothing when acting for no one; errors are ignored (the next run tries again).
pub fn give_back(dir: &Path, deep: bool) {
    #[cfg(unix)]
    if let Some(u) = current() {
        unix::give_back(u, dir, deep);
    }
    #[cfg(not(unix))]
    let _ = (dir, deep);
}

/// Makes `cmd` run as the user dfdu acts for, with their groups. Does nothing when acting for
/// no one.
pub fn run_as(cmd: &mut std::process::Command) {
    #[cfg(unix)]
    if let Some(u) = current() {
        unix::run_as(u, cmd);
    }
    #[cfg(not(unix))]
    let _ = cmd;
}

// The password and group database lookups and the identity change in a forked child.
#[cfg(unix)]
#[allow(unsafe_code)]
mod unix {
    use std::ffi::{CStr, OsStr, OsString};
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Path, PathBuf};

    use super::User;

    /// `getpwuid_r`, then `getgrouplist` for the supplementary groups.
    pub(super) fn lookup(uid: u32) -> Option<User> {
        let mut buf = vec![0u8; 16 * 1024];
        // SAFETY: `passwd` is plain old data; all-zero is a valid value to be overwritten.
        let mut pw: libc::passwd = unsafe { std::mem::zeroed() };
        let mut found: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: `pw` and `found` are valid for writes, `buf` for `buf.len()` bytes; the
        // strings `pw` points to live in `buf`, which outlives every read below.
        let r = unsafe {
            libc::getpwuid_r(
                uid,
                &raw mut pw,
                buf.as_mut_ptr().cast(),
                buf.len(),
                &raw mut found,
            )
        };
        if r != 0 || found.is_null() || pw.pw_name.is_null() || pw.pw_dir.is_null() {
            return None;
        }
        // SAFETY: both are non-null NUL-terminated strings in `buf` (checked above).
        let (name, dir) = unsafe { (CStr::from_ptr(pw.pw_name), CStr::from_ptr(pw.pw_dir)) };
        let home = PathBuf::from(OsStr::from_bytes(dir.to_bytes()));
        if !home.is_absolute() {
            return None;
        }
        let groups = groups(name, pw.pw_gid);
        Some(User {
            uid,
            gid: pw.pw_gid,
            name: OsString::from(OsStr::from_bytes(name.to_bytes())),
            home,
            groups,
        })
    }

    #[cfg(target_os = "macos")]
    type GroupT = libc::c_int;
    #[cfg(not(target_os = "macos"))]
    type GroupT = libc::gid_t;

    fn groups(name: &CStr, gid: u32) -> Vec<u32> {
        let mut n: libc::c_int = 64;
        loop {
            let mut list: Vec<GroupT> = vec![0; usize::try_from(n).unwrap_or(64)];
            let asked = n;
            // SAFETY: `name` is NUL-terminated; `list` is valid for writes of `n` entries and
            // `n` is updated with the count needed.
            let r = unsafe {
                libc::getgrouplist(name.as_ptr(), gid as GroupT, list.as_mut_ptr(), &raw mut n)
            };
            if r >= 0 {
                list.truncate(usize::try_from(n).unwrap_or(0));
                #[allow(clippy::cast_sign_loss, clippy::unnecessary_cast)]
                return list.into_iter().map(|g| g as u32).collect();
            }
            // Too small: `n` holds the count needed (Linux) or is unchanged (macOS).
            if n <= asked {
                n = asked.saturating_mul(2);
            }
            if n > 65_536 {
                return vec![gid];
            }
        }
    }

    /// The walk goes by file descriptor, never by path, because the user can change the tree
    /// while root walks it: each step is `openat(O_NOFOLLOW)` relative to the directory opened
    /// before it, the opened file is checked with `fstat`, and that same file is given back
    /// with `fchown`. So a directory swapped for a symlink is not entered, and what is changed
    /// is always what was checked. A regular file with more than one link is left alone: the
    /// user could have linked a root-owned file from elsewhere (macOS has no
    /// `protected_hardlinks`), and dfdu never makes hard links in its own directories.
    /// Symlinks, devices and other kinds of file are left alone too.
    pub(super) fn give_back(u: &User, dir: &Path, deep: bool) {
        use rustix::fs::Mode;
        use std::path::Component;
        let Ok(rel) = dir.strip_prefix(&u.home) else {
            return;
        };
        let mut names = Vec::new();
        for c in rel.components() {
            match c {
                Component::Normal(n) => names.push(n),
                Component::CurDir => {}
                _ => return,
            }
        }
        let Ok(mut fd) = rustix::fs::open(&u.home, dir_flags(), Mode::empty()) else {
            return;
        };
        let Ok(home) = rustix::fs::fstat(&fd) else {
            return;
        };
        for n in names {
            let Ok(next) = rustix::fs::openat(&fd, n, dir_flags(), Mode::empty()) else {
                return;
            };
            match check(u, &next, dev(&home)) {
                Some(Kind::Dir) => {}
                _ => return,
            }
            fd = next;
        }
        if deep {
            below(u, &fd, dev(&home), 0);
        }
    }

    const MAX_DEPTH: usize = 64;

    /// The device number, whose type differs between systems (`i32` on macOS).
    #[allow(
        clippy::cast_sign_loss,
        clippy::unnecessary_cast,
        clippy::useless_conversion
    )]
    fn dev(st: &rustix::fs::Stat) -> u64 {
        st.st_dev as u64
    }

    fn dir_flags() -> rustix::fs::OFlags {
        use rustix::fs::OFlags;
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
    }

    #[derive(PartialEq, Eq)]
    enum Kind {
        Dir,
        File,
    }

    /// What `fd` is, when it is a directory or a regular file with one link on `dev`; given
    /// back to the user (`fchown` on `fd` itself) when root owns it.
    fn check(u: &User, fd: &rustix::fd::OwnedFd, dev: u64) -> Option<Kind> {
        use rustix::fs::FileType;
        let st = rustix::fs::fstat(fd).ok()?;
        if self::dev(&st) != dev {
            return None;
        }
        let kind = match FileType::from_raw_mode(st.st_mode) {
            FileType::Directory => Kind::Dir,
            #[allow(clippy::useless_conversion, clippy::unnecessary_cast)]
            FileType::RegularFile if st.st_nlink as u64 == 1 => Kind::File,
            _ => return None,
        };
        if st.st_uid == 0 {
            let _ = rustix::fs::fchown(
                fd,
                Some(rustix::fs::Uid::from_raw(u.uid)),
                Some(rustix::fs::Gid::from_raw(u.gid)),
            );
        }
        Some(kind)
    }

    fn below(u: &User, dir: &rustix::fd::OwnedFd, dev: u64, depth: usize) {
        use rustix::fs::{Mode, OFlags};
        if depth >= MAX_DEPTH {
            return;
        }
        let Ok(entries) = rustix::fs::Dir::read_from(dir) else {
            return;
        };
        for e in entries.flatten() {
            let name = e.file_name();
            if name == c"." || name == c".." {
                continue;
            }
            // Opened without following a symlink and without blocking on a FIFO; a directory
            // and a file are told apart by `fstat`, not by the name.
            let flags = OFlags::RDONLY
                | OFlags::NOFOLLOW
                | OFlags::NONBLOCK
                | OFlags::NOCTTY
                | OFlags::CLOEXEC;
            let Ok(fd) = rustix::fs::openat(dir, name, flags, Mode::empty()) else {
                continue;
            };
            if check(u, &fd, dev) == Some(Kind::Dir) {
                below(u, &fd, dev, depth + 1);
            }
        }
    }

    pub(super) fn run_as(u: &User, cmd: &mut std::process::Command) {
        use std::os::unix::process::CommandExt;
        #[allow(clippy::unnecessary_cast)]
        let groups: Vec<libc::gid_t> = u.groups.iter().map(|&g| g as libc::gid_t).collect();
        let (uid, gid) = (u.uid, u.gid);
        let set = move || -> std::io::Result<()> {
            #[cfg(target_os = "macos")]
            let n = libc::c_int::try_from(groups.len()).unwrap_or(libc::c_int::MAX);
            #[cfg(not(target_os = "macos"))]
            let n = groups.len();
            // SAFETY: setgroups, setgid and setuid are async-signal-safe system calls; `groups`
            // was built before the fork and is only read, for `n` entries.
            unsafe {
                if libc::setgroups(n as _, groups.as_ptr()) != 0
                    || libc::setgid(gid) != 0
                    || libc::setuid(uid) != 0
                {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        };
        // SAFETY: the closure only makes the system calls above (no allocation, no locks), as
        // the child of a fork of a threaded process must.
        unsafe {
            cmd.pre_exec(set);
        }
        cmd.envs(u.env());
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn not_root_acts_for_no_one() {
        if rustix::process::geteuid().is_root() {
            return;
        }
        assert_eq!(detect(), None);
        assert_eq!(adopt().unwrap(), Adopted::Nobody);
        assert_eq!(current(), None);
        assert_eq!(owner_uid(), rustix::process::getuid().as_raw());
    }

    #[test]
    fn this_user_is_found() {
        let uid = rustix::process::getuid().as_raw();
        let u = unix::lookup(uid).expect("the password database knows this user");
        assert_eq!(u.uid, uid);
        assert!(u.home.is_absolute());
        assert!(u.groups.contains(&u.gid), "{:?}", u.groups);
    }
}

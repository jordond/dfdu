//! Acting for the user who started dfdu as root through `sudo` or `pkexec`, or for whom the
//! macOS helper daemon started it.
//!
//! A root dfdu scans as root, but everything that belongs to a person stays that person's:
//! configuration, rules, themes, the state directory (deletion journals, staging, history),
//! the scan cache, the home trash, and native commands, which run as them (`brew` refuses
//! root; `npm` or `cargo` run as root would leave root-owned files in their home).
//!
//! [`adopt`] is called first thing in `main`. When the process runs as root for another user
//! ([`detect`]: `SUDO_UID`, else `PKEXEC_UID`, else [`ACTING_UID_VAR`]), it makes `HOME`,
//! `USER` and `LOGNAME` theirs by executing itself again with them set when they differ (Linux
//! sudo resets `HOME` to root's; macOS keeps it, and the helper daemon has root's), so every
//! lookup of the user's directories, here and in the `trash` crate, finds theirs without the
//! process changing its own environment. Files dfdu creates as root in those directories are
//! given to the user by [`give_back`]; [`run_as`] makes a command run as them.
//!
//! A root process with none of these variables (a root login, `sudo -i` from root) acts for
//! no one.

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

/// The uid of the user an agent acts for, set by the macOS helper daemon (root) when it starts
/// the agent for a GUI: the euid of that GUI's process, as XPC reports it. Read only on macOS
/// and only when neither `SUDO_UID` nor `PKEXEC_UID` is set: those two are set by sudo and
/// pkexec themselves, while this one would pass through from the invoking user's environment
/// under `sudo -E` (or an `env_keep` rule) and must not let them choose whom root acts for. The
/// daemon's environment has neither.
pub const ACTING_UID_VAR: &str = "DFDU_ACTING_UID";

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

/// The user a root process runs for: the uid in `SUDO_UID`, else `PKEXEC_UID`, else (macOS)
/// [`ACTING_UID_VAR`], when it is not root's and the password database knows it. `None` when
/// not root.
#[cfg(unix)]
#[must_use]
pub fn detect() -> Option<User> {
    if !rustix::process::geteuid().is_root() {
        return None;
    }
    let uid = acting_uid(|v| std::env::var(v).ok())?;
    if uid == 0 {
        return None;
    }
    unix::lookup(uid)
}

/// The user with `uid`, from the password and group databases.
#[cfg(unix)]
#[must_use]
pub fn user(uid: u32) -> Option<User> {
    unix::lookup(uid)
}

/// This process's real uid.
#[cfg(unix)]
#[must_use]
pub fn this_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// The group of macOS administrators (`admin`), whose members may use the helper.
#[cfg(target_os = "macos")]
pub const ADMIN_GID: u32 = 80;

/// The uid in `SUDO_UID`, else `PKEXEC_UID`, else (macOS, the helper's agents)
/// [`ACTING_UID_VAR`], as `var` has them. A variable that is set decides, valid or not.
/// `SUDO_UID` comes first because sudo sets it and does not clear `PKEXEC_UID`, which under
/// `sudo -E` (or an `env_keep` rule) the invoking user could set to anyone; pkexec clears the
/// environment, so `PKEXEC_UID` is only trusted when sudo did not start the process.
#[cfg(unix)]
fn acting_uid(var: impl Fn(&str) -> Option<String>) -> Option<u32> {
    let names: &[&str] = if cfg!(target_os = "macos") {
        &["SUDO_UID", "PKEXEC_UID", ACTING_UID_VAR]
    } else {
        &["SUDO_UID", "PKEXEC_UID"]
    };
    let value = names.iter().find_map(|v| var(v))?;
    value.trim().parse::<u32>().ok()
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
    give_back_sealed(dir, deep, &[]);
}

/// [`give_back`], except that the directories named in `sealed` directly in `dir` are given
/// back themselves but never entered: what is in them keeps its owner. The staging directory is
/// one: a root-owned file staged from a root directory would otherwise become the user's, and
/// an undo would put it back there writable by them.
pub fn give_back_sealed(dir: &Path, deep: bool, sealed: &[&str]) {
    #[cfg(unix)]
    if let Some(u) = current() {
        unix::give_back(u, dir, deep, sealed);
    }
    #[cfg(not(unix))]
    let _ = (dir, deep, sealed);
}

/// Whether the deep walk enters the directory `name`, found `depth` levels below the directory
/// given back (0: directly in it): not when it is one of the `sealed` names there.
#[cfg_attr(not(unix), allow(dead_code))]
fn enters(depth: usize, name: &[u8], sealed: &[&str]) -> bool {
    depth != 0 || !sealed.iter().any(|s| s.as_bytes() == name)
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
    pub(super) fn give_back(u: &User, dir: &Path, deep: bool, sealed: &[&str]) {
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
            below(u, &fd, dev(&home), 0, sealed);
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

    fn below(u: &User, dir: &rustix::fd::OwnedFd, dev: u64, depth: usize, sealed: &[&str]) {
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
            if check(u, &fd, dev) == Some(Kind::Dir)
                && super::enters(depth, name.to_bytes(), sealed)
            {
                below(u, &fd, dev, depth + 1, sealed);
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
        // The user's own tools too (Homebrew's, which a root process must not look in: those
        // directories are the user's to write). Run as them, the command can do no more than
        // they can.
        let mut path: Vec<std::path::PathBuf> = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default();
        for extra in ["/opt/homebrew/bin", "/usr/local/bin"] {
            if !path.iter().any(|p| p.as_os_str() == extra) {
                path.push(extra.into());
            }
        }
        if let Ok(joined) = std::env::join_paths(path) {
            cmd.env("PATH", joined);
        }
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
    fn sudo_then_pkexec_decide_before_the_helpers_variable() {
        let env = |vars: &'static [(&'static str, &'static str)]| {
            move |k: &str| vars.iter().find(|v| v.0 == k).map(|v| v.1.to_owned())
        };
        // Passed through by `sudo -E`: ignored while sudo's own variable is there.
        assert_eq!(
            acting_uid(env(&[("SUDO_UID", "502"), (ACTING_UID_VAR, "501")])),
            Some(502)
        );
        // A `PKEXEC_UID` passed through `sudo -E` does not override sudo's own.
        assert_eq!(
            acting_uid(env(&[("SUDO_UID", "502"), ("PKEXEC_UID", "503")])),
            Some(502)
        );
        assert_eq!(
            acting_uid(env(&[("PKEXEC_UID", "503"), (ACTING_UID_VAR, "501")])),
            Some(503)
        );
        // A set variable that is not a uid acts for no one rather than falling through.
        assert_eq!(
            acting_uid(env(&[("SUDO_UID", "x"), (ACTING_UID_VAR, "501")])),
            None
        );
        assert_eq!(
            acting_uid(env(&[(ACTING_UID_VAR, "501")])),
            cfg!(target_os = "macos").then_some(501)
        );
        assert_eq!(acting_uid(env(&[])), None);
    }

    #[test]
    fn the_walk_does_not_enter_a_sealed_directory() {
        let sealed = ["stage"];
        // Given back itself (the walk checks it first), but not entered.
        assert!(!enters(0, b"stage", &sealed));
        assert!(enters(0, b"ops", &sealed));
        assert!(enters(0, b"stage", &[]));
        // Only directly in the directory given back.
        assert!(enters(1, b"stage", &sealed));
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

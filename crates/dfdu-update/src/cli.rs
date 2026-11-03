//! The CLI's release archive (`dfdu-<target>.tar.xz`, `.zip` on Windows): which one this
//! machine takes, the binary and the extras (man pages, completions) in it, and putting them in
//! place as install.sh does.

use std::io;
use std::path::{Path, PathBuf};

/// Releases are below these sizes; larger downloads, or binaries in them, are refused.
pub const MAX_ARCHIVE: u64 = 128 << 20;
pub const MAX_BINARY: u64 = 256 << 20;

/// The CLI's release target for this machine: the static (musl) build on Linux.
pub fn target() -> Option<&'static str> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "aarch64") => "aarch64-unknown-linux-musl",
        ("linux", "x86_64") => "x86_64-unknown-linux-musl",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        _ => return None,
    })
}

/// The CLI's archive for `target`.
pub fn asset(target: &str) -> String {
    if target.contains("windows") {
        format!("dfdu-{target}.zip")
    } else {
        format!("dfdu-{target}.tar.xz")
    }
}

/// The binary's file name for `target`.
pub fn bin_name(target: &str) -> &'static str {
    if target.contains("windows") {
        "dfdu.exe"
    } else {
        "dfdu"
    }
}

/// What a CLI archive holds.
#[derive(Debug, Default)]
pub struct Files {
    pub bin: Vec<u8>,
    /// Man pages and completions, by their path in the archive below its top directory
    /// (`man/dfdu.1`, `completions/_dfdu`).
    pub extras: Vec<(String, Vec<u8>)>,
}

/// The binary and the extras in the archive for `target`.
///
/// # Errors
/// The archive does not unpack, or holds no binary.
pub fn unpack(archive: &[u8], target: &str) -> io::Result<Files> {
    if target.contains("windows") {
        unzip(archive, target)
    } else {
        untar_xz(archive, target)
    }
}

#[cfg(feature = "net")]
fn untar_xz(archive: &[u8], target: &str) -> io::Result<Files> {
    use std::io::Read;
    let top = format!("dfdu-{target}");
    let xz = lzma_rust2::XzReader::new(archive, false);
    let mut tar = tar::Archive::new(xz);
    let mut files = Files::default();
    let mut found = false;
    for entry in tar.entries()? {
        let entry = entry?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry.path()?.into_owned();
        let Ok(rel) = path.strip_prefix(&top) else {
            continue;
        };
        let Some(rel) = rel.to_str().map(|r| r.replace('\\', "/")) else {
            continue;
        };
        let extra = extra_dest_kind(&rel).is_some();
        if rel != "dfdu" && !extra {
            continue;
        }
        if entry.size() > MAX_BINARY {
            return Err(io::Error::other(format!("{rel} in {top} is too large")));
        }
        let mut bytes = Vec::new();
        entry.take(MAX_BINARY).read_to_end(&mut bytes)?;
        if rel == "dfdu" {
            files.bin = bytes;
            found = true;
        } else {
            files.extras.push((rel, bytes));
        }
    }
    if !found {
        return Err(io::Error::other(format!(
            "the release archive has no {top}/dfdu"
        )));
    }
    Ok(files)
}

#[cfg(not(feature = "net"))]
fn untar_xz(_archive: &[u8], _target: &str) -> io::Result<Files> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "this dfdu was built without downloads",
    ))
}

/// Windows' archive is a zip, unpacked with the system's `tar.exe` (bsdtar, in Windows 10 1803
/// and later), which reads zips. Only `dfdu.exe` is taken, from the top or a `dfdu-<target>`
/// directory.
fn unzip(archive: &[u8], target: &str) -> io::Result<Files> {
    let dir = std::env::temp_dir().join(format!(
        "dfdu-update-{}-{}",
        std::process::id(),
        crate::sha256_hex(archive).get(..12).unwrap_or_default()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let r = (|| {
        let zip = dir.join("dfdu.zip");
        std::fs::write(&zip, archive)?;
        let out = dir.join("out");
        std::fs::create_dir(&out)?;
        let status = std::process::Command::new(system_tar())
            .arg("-xf")
            .arg(&zip)
            .arg("-C")
            .arg(&out)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|e| io::Error::new(e.kind(), format!("running tar to unzip: {e}")))?;
        if !status.success() {
            return Err(io::Error::other("tar could not unzip the release archive"));
        }
        let name = bin_name(target);
        for p in [
            out.join(name),
            out.join(format!("dfdu-{target}")).join(name),
        ] {
            if p.is_file() {
                if p.metadata()?.len() > MAX_BINARY {
                    return Err(io::Error::other(format!("{name} is too large")));
                }
                return Ok(Files {
                    bin: std::fs::read(&p)?,
                    extras: Vec::new(),
                });
            }
        }
        Err(io::Error::other(format!(
            "the release archive has no {name}"
        )))
    })();
    let _ = std::fs::remove_dir_all(&dir);
    r
}

/// `tar` from the system directory on Windows (never one earlier on `PATH`), else `tar`.
pub fn system_tar() -> PathBuf {
    if cfg!(windows)
        && let Some(root) = std::env::var_os("SystemRoot")
    {
        return Path::new(&root).join("System32").join("tar.exe");
    }
    PathBuf::from(if cfg!(target_os = "macos") {
        "/usr/bin/tar"
    } else {
        "tar"
    })
}

/// Puts `bin` at `dest` in one step: written beside it to a file of this process's own (never
/// one already there, nor through a link) and renamed over it, so a running copy is never
/// overwritten in place, nor two installs at once mixed. Windows cannot replace a running program, but can rename it:
/// the old one is moved aside to `.<name>.old` first, removed by [`remove_old`] later.
///
/// # Errors
/// The directory cannot be written.
pub fn install_bin(dest: &Path, bin: &[u8]) -> io::Result<()> {
    let dir = dest.parent().ok_or(io::ErrorKind::InvalidInput)?;
    let name = dest
        .file_name()
        .ok_or(io::ErrorKind::InvalidInput)?
        .to_string_lossy()
        .into_owned();
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{name}.new.{}", std::process::id()));
    // Left by an earlier process of the same id that died.
    let _ = std::fs::remove_file(&tmp);
    let written = (|| {
        let mut f = std::fs::File::options()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        io::Write::write_all(&mut f, bin)?;
        f.sync_all()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(std::fs::Permissions::from_mode(0o755))?;
        }
        Ok(())
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    let old = dir.join(format!(".{name}.old"));
    let aside = cfg!(windows) && dest.exists();
    if aside {
        remove_old(dest);
        // An old copy still running cannot be removed; leave it under another name.
        let old = if old.exists() {
            dir.join(format!(".{name}.old.{}", std::process::id()))
        } else {
            old.clone()
        };
        if let Err(e) = std::fs::rename(dest, &old) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        if let Err(e) = std::fs::rename(&tmp, dest) {
            let _ = std::fs::rename(&old, dest);
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        return Ok(());
    }
    std::fs::rename(&tmp, dest).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Removes what [`install_bin`] moved aside beside `dest`, where it can.
pub fn remove_old(dest: &Path) {
    let (Some(dir), Some(name)) = (dest.parent(), dest.file_name()) else {
        return;
    };
    let prefix = format!(".{}.old", name.to_string_lossy());
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        if e.file_name().to_string_lossy().starts_with(&prefix) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Where an extra goes, as install.sh puts it: `(data or config, path below it)`.
fn extra_dest_kind(rel: &str) -> Option<(bool, String)> {
    if let Some(page) = rel.strip_prefix("man/")
        && page.ends_with(".1")
        && !page.contains('/')
    {
        return Some((true, format!("man/man1/{page}")));
    }
    Some(match rel {
        "completions/dfdu.bash" => (true, "bash-completion/completions/dfdu".into()),
        "completions/_dfdu" => (true, "zsh/site-functions/_dfdu".into()),
        "completions/dfdu.fish" => (false, "fish/completions/dfdu.fish".into()),
        _ => return None,
    })
}

/// The per-user directories install.sh puts the extras in: `XDG_DATA_HOME` (else
/// `~/.local/share`) and `XDG_CONFIG_HOME` (else `~/.config`), on macOS too.
#[derive(Clone, Debug)]
pub struct ExtraDirs {
    pub data: PathBuf,
    pub config: PathBuf,
}

impl ExtraDirs {
    pub fn from_env() -> Option<Self> {
        let var = |name| {
            std::env::var_os(name)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        let home = dfdu_sys::dirs::home_dir();
        Some(ExtraDirs {
            data: var("XDG_DATA_HOME").or_else(|| Some(home.as_ref()?.join(".local/share")))?,
            config: var("XDG_CONFIG_HOME").or_else(|| Some(home.as_ref()?.join(".config")))?,
        })
    }

    /// Where `rel` (as [`Files::extras`] names it) goes.
    pub fn dest(&self, rel: &str) -> Option<PathBuf> {
        let (data, below) = extra_dest_kind(rel)?;
        Some(if data { &self.data } else { &self.config }.join(below))
    }

    /// Writes `extras`. With `only_existing` (an update of what install.sh installed), only
    /// the kinds already there: every man page once one of dfdu's is (so a new command's page
    /// arrives too), and each completion over its own file. Returns the paths written; one
    /// that cannot be is skipped.
    pub fn install(&self, extras: &[(String, Vec<u8>)], only_existing: bool) -> Vec<PathBuf> {
        let man1 = self.data.join("man").join("man1");
        let has_man = std::fs::read_dir(&man1).is_ok_and(|entries| {
            entries.flatten().any(|e| {
                let name = e.file_name();
                let name = name.to_string_lossy();
                name.starts_with("dfdu") && name.ends_with(".1")
            })
        });
        let mut written = Vec::new();
        for (rel, bytes) in extras {
            let Some(dest) = self.dest(rel) else { continue };
            let there = if rel.starts_with("man/") {
                has_man
            } else {
                dest.is_file()
            };
            if only_existing && !there {
                continue;
            }
            let ok = dest
                .parent()
                .is_some_and(|d| std::fs::create_dir_all(d).is_ok())
                && std::fs::write(&dest, bytes).is_ok();
            if ok {
                written.push(dest);
            }
        }
        written
    }
}

/// What manages a copy of dfdu, so that it is updated through it and not replaced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Managed {
    Homebrew,
    Nix,
    Scoop,
    Winget,
    /// `cargo install`, from crates.io or git.
    Cargo,
    /// A system directory (`/usr/bin`, MacPorts' `/opt/local/bin`): a package manager.
    System,
}

impl Managed {
    /// What manages the program at `exe` (symlinks resolved), if anything does.
    pub fn of(exe: &Path) -> Option<Managed> {
        let exe = std::fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf());
        let s = exe.to_string_lossy().replace('\\', "/");
        if s.contains("/Cellar/")
            || s.contains("/Caskroom/")
            || s.starts_with("/opt/homebrew/")
            || s.starts_with("/home/linuxbrew/.linuxbrew/")
        {
            return Some(Managed::Homebrew);
        }
        if s.starts_with("/nix/store/") {
            return Some(Managed::Nix);
        }
        // Windows paths, compared without case.
        let lower = s.to_ascii_lowercase();
        if lower.contains("/scoop/apps/") || lower.contains("/scoop/shims/") {
            return Some(Managed::Scoop);
        }
        if lower.contains("/microsoft/winget/packages/")
            || lower.contains("/microsoft/winget/links/")
        {
            return Some(Managed::Winget);
        }
        if installed_by_cargo(&exe) {
            return Some(Managed::Cargo);
        }
        let dir = s.rsplit_once('/').map_or("", |(dir, _)| dir);
        [
            "/usr/bin",
            "/bin",
            "/usr/sbin",
            "/sbin",
            "/usr/libexec",
            "/opt/local/bin",
        ]
        .contains(&dir)
        .then_some(Managed::System)
    }

    /// What to do instead of replacing the CLI.
    pub fn advice(self) -> &'static str {
        match self {
            Managed::Homebrew => "installed with Homebrew; update it with: brew upgrade dfdu",
            Managed::Nix => {
                "installed with Nix; update it through Nix (nix profile upgrade, or the flake input)"
            }
            Managed::Scoop => "installed with Scoop; update it with: scoop update dfdu",
            Managed::Winget => "installed with winget; update it with: winget upgrade jordond.dfdu",
            Managed::Cargo => "installed with cargo; update it with: cargo install --locked dfdu",
            Managed::System => "installed by a package manager; update it with that",
        }
    }
}

/// Whether `cargo install` put the program at `exe` there: it is in a `bin` directory whose
/// parent (cargo's home) records a `dfdu` package in `.crates2.json`. dist's installers use the
/// same `bin` directory but don't write that record.
fn installed_by_cargo(exe: &Path) -> bool {
    let Some(bin) = exe.parent() else {
        return false;
    };
    if bin.file_name().is_none_or(|n| n != "bin") {
        return false;
    }
    let Some(home) = bin.parent() else {
        return false;
    };
    // Keys look like "dfdu 0.3.0 (registry+https://github.com/rust-lang/crates.io-index)".
    std::fs::read_to_string(home.join(".crates2.json")).is_ok_and(|s| s.contains("\"dfdu "))
}

#[cfg(test)]
mod tests;

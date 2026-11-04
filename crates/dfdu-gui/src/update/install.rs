//! Installing: a newer GUI over this one, as its platform's package allows, and the CLI of this
//! GUI's version. Each runs on a thread of its own (`super`); nothing here draws.
//!
//! - macOS: `dfdu-gui-<target>.app.tar.gz` unpacked beside the running app and swapped in by
//!   two renames, as install.sh does; a restart opens the new one. An app from the Homebrew
//!   cask is left to `brew upgrade --cask`.
//! - Linux: the AppImage (`$APPIMAGE`) replaced by the new one; the .deb's copy is left to the
//!   package manager.
//! - Windows: the installer (`-setup.exe`) downloaded and started, for a copy it installed
//!   (with `uninstall.exe` beside it); the window then closes so it can replace it. The zip's
//!   copy is left to the user.
//!
//! The CLI goes to `~/.local/bin` with its man pages and completions where install.sh puts
//! them, or on Windows to `%LOCALAPPDATA%\Programs\dfdu`, added to the user's `PATH`.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use dfdu_update::cli::{self, ExtraDirs};
use dfdu_update::{Releases, Version};

/// GUI downloads are below this size.
const MAX_GUI: u64 = 512 << 20;

/// How this copy of the GUI is updated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum How {
    /// macOS: the app bundle at this path is replaced.
    #[cfg_attr(
        not(target_os = "macos"),
        expect(dead_code, reason = "only macOS runs app bundles")
    )]
    App(PathBuf),
    /// Linux: the AppImage at this path is replaced.
    #[cfg_attr(
        not(any(target_os = "linux", test)),
        expect(dead_code, reason = "only Linux runs AppImages")
    )]
    AppImage(PathBuf),
    /// Windows: the installer is run.
    Setup,
    /// By a package manager, with this command.
    Command(&'static str),
    /// By hand, from the release page (a copy outside any package, or a build from source).
    Manual,
}

impl How {
    /// How this copy is updated.
    pub fn detect() -> How {
        let Ok(exe) = std::env::current_exe() else {
            return How::Manual;
        };
        detect(&exe)
    }
}

#[cfg(target_os = "macos")]
fn detect(exe: &Path) -> How {
    let cask = [
        "/opt/homebrew/Caskroom/dfdu-gui",
        "/usr/local/Caskroom/dfdu-gui",
    ];
    if cask.iter().any(|c| Path::new(c).is_dir()) {
        return How::Command("brew upgrade --cask dfdu-gui");
    }
    match exe
        .ancestors()
        .find(|a| a.extension().is_some_and(|e| e == "app"))
    {
        Some(app) if cli::target().is_some() => How::App(app.to_path_buf()),
        _ => How::Manual,
    }
}

#[cfg(target_os = "linux")]
fn detect(exe: &Path) -> How {
    let var = |name| std::env::var_os(name).map(PathBuf::from);
    if let (Some(image), Some(dir)) = (var("APPIMAGE"), var("APPDIR"))
        && appimage(exe, &image, &dir)
        && appimage_asset(std::env::consts::ARCH).is_some()
    {
        return How::AppImage(image);
    }
    if exe.starts_with("/usr/bin") {
        return How::Command(if cfg!(target_arch = "aarch64") {
            "download the .deb from the release page, then: sudo apt install ./dfdu-gui-aarch64-unknown-linux-gnu.deb"
        } else {
            "download the .deb from the release page, then: sudo apt install ./dfdu-gui-x86_64-unknown-linux-gnu.deb"
        });
    }
    How::Manual
}

/// The release's AppImage for a Linux machine of architecture `arch`, if there is one.
fn appimage_asset(arch: &str) -> Option<&'static str> {
    match arch {
        "x86_64" => Some("dfdu-gui-x86_64-unknown-linux-gnu.AppImage"),
        "aarch64" => Some("dfdu-gui-aarch64-unknown-linux-gnu.AppImage"),
        _ => None,
    }
}

/// Whether `exe` runs from the AppImage `image`, mounted at `dir`. Both variables are passed
/// down to every program an AppImage starts, so another AppImage's (one that started this
/// copy) are told apart by where this program is.
#[cfg_attr(
    not(any(target_os = "linux", test)),
    expect(dead_code, reason = "only Linux runs AppImages")
)]
fn appimage(exe: &Path, image: &Path, dir: &Path) -> bool {
    image.is_file() && !dir.as_os_str().is_empty() && exe.starts_with(dir)
}

#[cfg(windows)]
fn detect(exe: &Path) -> How {
    let lower = exe
        .to_string_lossy()
        .to_ascii_lowercase()
        .replace('\\', "/");
    if lower.contains("/scoop/apps/") {
        return How::Command("scoop update dfdu-gui");
    }
    let installed = exe
        .parent()
        .is_some_and(|d| d.join("uninstall.exe").is_file());
    if installed && std::env::consts::ARCH == "x86_64" {
        How::Setup
    } else {
        How::Manual
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn detect(_exe: &Path) -> How {
    How::Manual
}

/// What an update left to do.
#[derive(Debug)]
pub enum Installed {
    /// Start this program, the new copy, and close the window.
    Restart(PathBuf),
    /// The installer is running: close the window so it can replace it.
    Close,
}

/// Downloads release `v` and installs it as `how` says.
pub fn gui(how: &How, releases: &Releases, v: &Version) -> Result<Installed, String> {
    match how {
        How::App(app) => {
            let target = cli::target().ok_or("there is no dfdu build for this Mac")?;
            let name = format!("dfdu-gui-{target}.app.tar.gz");
            let tarball = releases.fetch(v, &name, MAX_GUI).map_err(text)?;
            swap_app(app, &tarball).map_err(|e| format!("replacing {}: {e}", app.display()))?;
            Ok(Installed::Restart(app.clone()))
        }
        How::AppImage(image) => {
            let name = appimage_asset(std::env::consts::ARCH)
                .ok_or("there is no dfdu AppImage for this machine")?;
            let bytes = releases.fetch(v, name, MAX_GUI).map_err(text)?;
            cli::install_bin(image, &bytes)
                .map_err(|e| format!("replacing {}: {e}", image.display()))?;
            Ok(Installed::Restart(image.clone()))
        }
        How::Setup => {
            let name = "dfdu-gui-x86_64-pc-windows-msvc-setup.exe";
            let bytes = releases.fetch(v, name, MAX_GUI).map_err(text)?;
            let path = std::env::temp_dir().join(format!("dfdu-gui-{v}-setup.exe"));
            std::fs::write(&path, &bytes).map_err(|e| format!("{}: {e}", path.display()))?;
            Command::new(&path)
                .spawn()
                .map_err(|e| format!("starting the installer: {e}"))?;
            Ok(Installed::Close)
        }
        How::Command(_) | How::Manual => Err("this copy of dfdu is not updated from here".into()),
    }
}

fn text(e: io::Error) -> String {
    e.to_string()
}

/// Unpacks `tarball` (holding `dfdu.app`) beside `app` and puts it in its place: the old app is
/// renamed aside, the new one renamed in, and the old one removed. A failure puts the old one
/// back.
fn swap_app(app: &Path, tarball: &[u8]) -> io::Result<()> {
    let dir = app.parent().ok_or(io::ErrorKind::InvalidInput)?;
    let work = dir.join(format!(".dfdu-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir(&work)?;
    let r = (|| {
        let file = work.join("dfdu.app.tar.gz");
        std::fs::write(&file, tarball)?;
        let ok = Command::new(cli::system_tar())
            .arg("-xzf")
            .arg(&file)
            .arg("-C")
            .arg(&work)
            .status()?
            .success();
        let new = work.join("dfdu.app");
        if !ok || !new.join("Contents").is_dir() {
            return Err(io::Error::other("the download holds no dfdu.app"));
        }
        let old = work.join("old.app");
        std::fs::rename(app, &old)?;
        if let Err(e) = std::fs::rename(&new, app) {
            let _ = std::fs::rename(&old, app);
            return Err(e);
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&work);
    r
}

/// Starts `program`, the new copy: `open -n` for an app bundle.
pub fn restart(program: &Path) -> io::Result<()> {
    if program.extension().is_some_and(|e| e == "app") {
        Command::new("/usr/bin/open")
            .arg("-n")
            .arg(program)
            .spawn()?;
    } else {
        Command::new(program).spawn()?;
    }
    Ok(())
}

/// Where the CLI is installed from here.
pub fn cli_dest() -> Option<PathBuf> {
    let name = cli::bin_name(cli::target()?);
    if cfg!(windows) {
        let local = std::env::var_os("LOCALAPPDATA")?;
        Some(Path::new(&local).join("Programs").join("dfdu").join(name))
    } else {
        Some(
            dfdu_sys::dirs::home_dir()?
                .join(".local")
                .join("bin")
                .join(name),
        )
    }
}

/// A CLI found on this machine.
#[derive(Clone, Debug)]
pub struct Found {
    pub path: PathBuf,
    /// What its `--version` printed (`dfdu 0.3.0`).
    pub version: Option<String>,
}

/// The `dfdu` a shell would most likely run: the first on `PATH`, else in the usual places
/// (a window opened from the desktop often has a shorter `PATH` than a terminal).
pub fn find_cli() -> Option<Found> {
    let target = cli::target()?;
    let name = cli::bin_name(target);
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    dirs.extend(cli_dest().and_then(|d| d.parent().map(Path::to_path_buf)));
    if let Some(home) = dfdu_sys::dirs::home_dir() {
        dirs.push(home.join(".cargo").join("bin"));
    }
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from));
    let path = dirs
        .into_iter()
        .map(|d| d.join(name))
        .find(|p| p.is_file())?;
    let version = quiet(Command::new(&path).arg("--version"))
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .filter(|v| !v.is_empty());
    Some(Found { path, version })
}

/// No console window for a console program started from the window on Windows.
fn quiet(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.stdin(std::process::Stdio::null())
}

/// What installing the CLI did.
#[derive(Clone, Debug)]
pub struct CliInstalled {
    pub path: PathBuf,
    /// Man pages and completions written.
    pub extras: usize,
    /// The line to add to the shell's profile when its directory is not on `PATH`, and the
    /// profile.
    pub path_hint: Option<(String, String)>,
}

/// Downloads the CLI of release `v` and installs it at [`cli_dest`], with the extras in the
/// per-user directories (not on Windows).
pub fn cli(releases: &Releases, v: &Version) -> Result<CliInstalled, String> {
    let dest = cli_dest().ok_or("there is no home directory to install into")?;
    let extras = ExtraDirs::from_env().filter(|_| cfg!(unix));
    cli_into(releases, v, &dest, extras.as_ref())
}

/// [`cli`] into `dest`, with the extras in `extras`.
fn cli_into(
    releases: &Releases,
    v: &Version,
    dest: &Path,
    extras: Option<&ExtraDirs>,
) -> Result<CliInstalled, String> {
    let target = cli::target().ok_or("there is no dfdu build for this platform")?;
    let asset = cli::asset(target);
    let archive = releases.fetch(v, &asset, cli::MAX_ARCHIVE).map_err(text)?;
    let files = cli::unpack(&archive, target).map_err(|e| format!("{asset}: {e}"))?;
    if cfg!(windows) {
        cli::remove_old(dest);
    }
    cli::install_bin(dest, &files.bin).map_err(|e| format!("{}: {e}", dest.display()))?;
    let extras = extras.map_or(0, |d| d.install(&files.extras, false).len());
    let dir = dest.parent().map(Path::to_path_buf).unwrap_or_default();
    let path_hint = if cfg!(windows) {
        add_to_user_path(&dir).map_err(|e| format!("adding {} to PATH: {e}", dir.display()))?;
        None
    } else {
        let on_path =
            std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d == dir));
        (!on_path).then(|| path_hint(&dir))
    };
    let dest = dest.to_path_buf();
    Ok(CliInstalled {
        path: dest,
        extras,
        path_hint,
    })
}

/// The line that puts `dir` on `PATH` for the user's shell, and the file it goes in, as
/// install.sh says it.
fn path_hint(dir: &Path) -> (String, String) {
    let home = dfdu_sys::dirs::home_dir();
    let shown = match home.as_deref().and_then(|h| dir.strip_prefix(h).ok()) {
        Some(rel) => format!("$HOME/{}", rel.display()),
        None => dir.display().to_string(),
    };
    let shell = std::env::var("SHELL").unwrap_or_default();
    let shell = shell.rsplit('/').next().unwrap_or_default();
    let export = format!("export PATH=\"{shown}:$PATH\"");
    match shell {
        "fish" => (
            format!("fish_add_path {shown}"),
            "~/.config/fish/config.fish".into(),
        ),
        "bash" if cfg!(target_os = "macos") => (export, "~/.bash_profile".into()),
        "bash" => (export, "~/.bashrc".into()),
        // zsh is macOS's default shell, and the window may not see SHELL there.
        "zsh" => (export, "~/.zshrc".into()),
        "" if cfg!(target_os = "macos") => (export, "~/.zshrc".into()),
        _ => (export, "your shell's profile".into()),
    }
}

/// Adds `dir` to the user's `PATH` unless it is there, through the system's PowerShell. The
/// registry value is read unexpanded and written back as `REG_EXPAND_SZ`, so the entries that
/// name variables (`%USERPROFILE%\...`) stay as they are; `SetEnvironmentVariable` would expand
/// them and write a plain string. Removing a variable that is not there then tells running
/// programs that the environment changed, so new terminals see it.
#[cfg(windows)]
fn add_to_user_path(dir: &Path) -> io::Result<()> {
    const SCRIPT: &str = "$ErrorActionPreference = 'Stop'; \
        $d = $env:DFDU_PATH_DIR; \
        $k = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true); \
        $p = $k.GetValue('Path', '', \
            [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames); \
        if (($p -split ';') -notcontains $d) { \
            $n = if ($p) { $p.TrimEnd(';') + ';' + $d } else { $d }; \
            $k.SetValue('Path', $n, [Microsoft.Win32.RegistryValueKind]::ExpandString); \
            [Environment]::SetEnvironmentVariable('DFDU_PATH_DIR', $null, 'User') }; \
        $k.Close()";
    let root = std::env::var_os("SystemRoot").ok_or_else(|| io::Error::other("no SystemRoot"))?;
    let powershell = Path::new(&root)
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    let status = quiet(
        Command::new(powershell)
            .args(["-NoProfile", "-NonInteractive", "-Command", SCRIPT])
            .env("DFDU_PATH_DIR", dir),
    )
    .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other("PowerShell could not change it"))
    }
}

#[cfg(not(windows))]
fn add_to_user_path(_dir: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::super::tests::tar_xz;
    use super::super::tests::{asset, serve};
    use super::*;

    #[test]
    fn an_appimage_is_this_programs_only_when_it_runs_from_its_mount() {
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("dfdu.AppImage");
        std::fs::write(&image, "image").unwrap();
        let mount = Path::new("/tmp/.mount_dfduXYZ");
        assert!(appimage(&mount.join("usr/bin/dfdu-gui"), &image, mount));
        // Started by another AppImage, which passed its variables down.
        assert!(!appimage(Path::new("/usr/bin/dfdu-gui"), &image, mount));
        assert!(!appimage(
            &mount.join("usr/bin/dfdu-gui"),
            &dir.path().join("gone"),
            mount
        ));
        assert!(!appimage(
            Path::new("/usr/bin/dfdu-gui"),
            &image,
            Path::new("")
        ));
    }

    #[test]
    fn each_linux_architecture_has_its_own_appimage() {
        assert_eq!(
            appimage_asset("x86_64"),
            Some("dfdu-gui-x86_64-unknown-linux-gnu.AppImage")
        );
        assert_eq!(
            appimage_asset("aarch64"),
            Some("dfdu-gui-aarch64-unknown-linux-gnu.AppImage")
        );
        assert_eq!(appimage_asset("riscv64"), None);
    }

    #[test]
    fn an_appimage_is_replaced_by_the_release_one() {
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("dfdu-gui");
        std::fs::write(&image, "old").unwrap();
        let name = appimage_asset(std::env::consts::ARCH).unwrap();
        let base = serve(asset("9.9.9", name, b"new image".to_vec()));
        let v = Version::parse("9.9.9").unwrap();
        let how = How::AppImage(image.clone());
        let done = gui(&how, &Releases::new(&base), &v).unwrap();
        assert!(matches!(done, Installed::Restart(p) if p == image));
        assert_eq!(std::fs::read(&image).unwrap(), b"new image");

        // A download that does not match its checksum leaves the old one.
        let mut bad = asset("9.9.9", name, b"bad".to_vec());
        bad[0].1 = format!("{}  x\n", "0".repeat(64)).into_bytes();
        let e = gui(&how, &Releases::new(&serve(bad)), &v).unwrap_err();
        assert!(e.contains("does not match"), "{e}");
        assert_eq!(std::fs::read(&image).unwrap(), b"new image");
    }

    #[cfg(unix)]
    #[test]
    fn the_cli_is_installed_with_its_extras() {
        let target = cli::target().unwrap();
        let archive = tar_xz(&[
            (&format!("dfdu-{target}/dfdu"), b"cli"),
            (&format!("dfdu-{target}/man/dfdu.1"), b"man"),
            (&format!("dfdu-{target}/completions/_dfdu"), b"zsh"),
        ]);
        let base = serve(asset("9.9.9", &cli::asset(target), archive));
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("bin").join("dfdu");
        let extras = ExtraDirs {
            data: dir.path().join("share"),
            config: dir.path().join("config"),
        };
        let v = Version::parse("9.9.9").unwrap();
        let done = cli_into(&Releases::new(&base), &v, &dest, Some(&extras)).unwrap();
        assert_eq!(done.path, dest);
        assert_eq!(done.extras, 2);
        assert_eq!(std::fs::read(&dest).unwrap(), b"cli");
        assert!(dir.path().join("share/man/man1/dfdu.1").is_file());
        // A temporary directory is not on PATH: the window says how to put it there.
        assert!(done.path_hint.is_some());
    }

    #[test]
    fn the_path_hint_matches_the_shell() {
        let dir = Path::new("/opt/dfdu/bin");
        let (line, _) = path_hint(dir);
        assert!(line.contains("/opt/dfdu/bin"), "{line}");
    }

    #[test]
    fn the_cli_goes_beside_the_users_programs() {
        let dest = cli_dest().expect("a home directory");
        if cfg!(windows) {
            assert!(
                dest.ends_with("Programs/dfdu/dfdu.exe"),
                "{}",
                dest.display()
            );
        } else {
            assert!(dest.ends_with(".local/bin/dfdu"), "{}", dest.display());
        }
    }

    #[cfg(unix)]
    #[test]
    fn an_app_is_swapped_for_the_one_downloaded() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("dfdu.app");
        std::fs::create_dir_all(app.join("Contents")).unwrap();
        std::fs::write(app.join("Contents/old"), "old").unwrap();
        // The release's tarball: dfdu.app/Contents/new.
        let src = dir.path().join("src");
        std::fs::create_dir_all(src.join("dfdu.app/Contents")).unwrap();
        std::fs::write(src.join("dfdu.app/Contents/new"), "new").unwrap();
        let tarball = dir.path().join("t.tar.gz");
        let ok = Command::new(cli::system_tar())
            .arg("-czf")
            .arg(&tarball)
            .arg("-C")
            .arg(&src)
            .arg("dfdu.app")
            .status()
            .unwrap()
            .success();
        assert!(ok);
        swap_app(&app, &std::fs::read(&tarball).unwrap()).unwrap();
        assert!(app.join("Contents/new").is_file());
        assert!(!app.join("Contents/old").exists());
        // Nothing left beside it.
        let mut left: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["dfdu.app", "src", "t.tar.gz"]);

        // A download without an app leaves the old one.
        let e = swap_app(&app, b"not a tarball").unwrap_err();
        assert!(app.join("Contents/new").is_file(), "{e}");
    }
}

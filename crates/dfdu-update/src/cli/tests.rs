#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use super::*;

#[test]
fn every_release_target_has_an_archive() {
    assert_eq!(
        asset("aarch64-apple-darwin"),
        "dfdu-aarch64-apple-darwin.tar.xz"
    );
    assert_eq!(
        asset("x86_64-pc-windows-msvc"),
        "dfdu-x86_64-pc-windows-msvc.zip"
    );
    assert_eq!(bin_name("x86_64-pc-windows-msvc"), "dfdu.exe");
    assert_eq!(bin_name("x86_64-unknown-linux-musl"), "dfdu");
}

#[test]
fn a_binary_is_replaced_whole() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("bin").join("dfdu");
    install_bin(&dest, b"one").unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), b"one");
    install_bin(&dest, b"two").unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), b"two");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = dest.metadata().unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
    }
    // Nothing left beside it.
    remove_old(&dest);
    let left: Vec<_> = std::fs::read_dir(dest.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(left, ["dfdu"]);
}

#[test]
fn extras_go_where_install_sh_puts_them() {
    let dir = tempfile::tempdir().unwrap();
    let dirs = ExtraDirs {
        data: dir.path().join("share"),
        config: dir.path().join("config"),
    };
    let rel = |p: &Path| p.strip_prefix(dir.path()).unwrap().to_path_buf();
    let extras = vec![
        ("man/dfdu.1".to_owned(), b"man".to_vec()),
        ("completions/dfdu.bash".to_owned(), b"bash".to_vec()),
        ("completions/_dfdu".to_owned(), b"zsh".to_vec()),
        ("completions/dfdu.fish".to_owned(), b"fish".to_vec()),
        ("completions/dfdu.elv".to_owned(), b"elvish".to_vec()),
    ];
    // An update writes only over what is there.
    assert!(dirs.install(&extras, true).is_empty());
    let written: Vec<_> = dirs
        .install(&extras, false)
        .iter()
        .map(|p| rel(p))
        .collect();
    let want: Vec<std::path::PathBuf> = [
        "share/man/man1/dfdu.1",
        "share/bash-completion/completions/dfdu",
        "share/zsh/site-functions/_dfdu",
        "config/fish/completions/dfdu.fish",
    ]
    .iter()
    .map(|p| p.split('/').collect())
    .collect();
    assert_eq!(written, want);
    std::fs::remove_file(dir.path().join("share/man/man1/dfdu.1")).unwrap();
    assert_eq!(dirs.install(&extras, true).len(), 3);

    // A new command's man page arrives once any of dfdu's pages is there.
    std::fs::write(dir.path().join("share/man/man1/dfdu-scan.1"), "old").unwrap();
    let pages = vec![
        ("man/dfdu.1".to_owned(), b"man".to_vec()),
        ("man/dfdu-update.1".to_owned(), b"new".to_vec()),
    ];
    assert_eq!(dirs.install(&pages, true).len(), 2);
    assert!(dir.path().join("share/man/man1/dfdu-update.1").is_file());
}

#[test]
fn a_link_at_the_temporary_name_is_not_written_through() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("dfdu");
    let target = dir.path().join("elsewhere");
    std::fs::write(&target, "keep").unwrap();
    // Another process's temporary file, or a planted link, is left alone.
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        &target,
        dir.path().join(format!(".dfdu.new.{}", std::process::id())),
    )
    .unwrap();
    install_bin(&dest, b"new").unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), b"new");
    assert_eq!(std::fs::read(&target).unwrap(), b"keep");
}

/// Windows' CLI archive is a zip, read by the system's bsdtar (macOS has one too).
#[cfg(any(target_os = "macos", windows))]
#[test]
fn a_zip_unpacks_to_its_binary() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir(&src).unwrap();
    std::fs::write(src.join("dfdu.exe"), b"MZ binary").unwrap();
    std::fs::write(src.join("README.md"), b"read me").unwrap();
    let zip = dir.path().join("a.zip");
    let ok = std::process::Command::new(system_tar())
        .args(["-a", "-cf"])
        .arg(&zip)
        .arg("-C")
        .arg(&src)
        .args(["dfdu.exe", "README.md"])
        .status()
        .unwrap()
        .success();
    assert!(ok);
    let files = unpack(&std::fs::read(&zip).unwrap(), "x86_64-pc-windows-msvc").unwrap();
    assert_eq!(files.bin, b"MZ binary");
    assert!(unpack(b"not a zip", "x86_64-pc-windows-msvc").is_err());
}

#[test]
fn package_managers_are_recognised_by_path() {
    let m = |p: &str| Managed::of(Path::new(p));
    assert_eq!(m("/opt/homebrew/bin/dfdu"), Some(Managed::Homebrew));
    assert_eq!(
        m("/usr/local/Cellar/dfdu/0.3.0/bin/dfdu"),
        Some(Managed::Homebrew)
    );
    assert_eq!(
        m("/home/linuxbrew/.linuxbrew/bin/dfdu"),
        Some(Managed::Homebrew)
    );
    assert_eq!(m("/nix/store/abc-dfdu-0.3.0/bin/dfdu"), Some(Managed::Nix));
    assert_eq!(m("/usr/bin/dfdu"), Some(Managed::System));
    assert_eq!(m("/opt/local/bin/dfdu"), Some(Managed::System));
    assert_eq!(
        m(r"C:\Users\u\scoop\apps\dfdu\0.3.0\dfdu.exe"),
        Some(Managed::Scoop)
    );
    assert_eq!(
        m(r"C:\Users\u\AppData\Local\Microsoft\WinGet\Packages\jordond.dfdu_x\dfdu.exe"),
        Some(Managed::Winget)
    );
    assert_eq!(m(r"C:\Users\u\AppData\Local\Programs\dfdu\dfdu.exe"), None);
    assert_eq!(m("/usr/local/bin/dfdu"), None);
    assert_eq!(m("/home/u/.local/bin/dfdu"), None);
    assert_eq!(m("/home/u/.cargo/bin/dfdu"), None);
}

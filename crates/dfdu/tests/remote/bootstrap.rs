//! `--remote` through a stand-in for ssh that runs the remote command with this machine's `sh`
//! (as the login shell on the host would) in a home of its own, without dfdu installed.

use super::common;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output, Stdio};

/// `dfdu args` on `home` through the stand-in for ssh, with `--remote-agent mode`.
/// `DFDU_REMOTE_AGENT` is left out of the environment.
fn cmd(home: &Path, args: &[&str], mode: Option<&str>) -> Command {
    let ssh = home.join("ssh");
    if !ssh.exists() {
        // ssh joins its arguments after `-- HOST` and hands them to the login shell.
        std::fs::write(&ssh, "#!/bin/sh\nshift 2\nexec sh -c \"$*\"\n").unwrap();
        std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut c = common::bin_in(&common::shared_state());
    c.args(args)
        .args(["--remote", "elsewhere"])
        .args(mode.map(|m| ["--remote-agent", m]).into_iter().flatten())
        .env_remove("DFDU_REMOTE_COMMAND")
        .env_remove("DFDU_REMOTE_AGENT")
        .env_remove("DFDU_AGENT_DIR")
        .env_remove("XDG_CACHE_HOME")
        .env_remove("XDG_RUNTIME_DIR")
        .env("DFDU_REMOTE_SSH", &ssh)
        .env("DFDU_RELEASE_URL", "http://127.0.0.1:1")
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin");
    c
}

fn run(home: &Path, args: &[&str], mode: &str) -> Output {
    cmd(home, args, Some(mode)).output().unwrap()
}

#[test]
fn sends_the_agent_once_and_scans_with_it() {
    let tmp = tempfile::tempdir().unwrap();
    let home = dunce::canonicalize(tmp.path()).unwrap();
    let root = home.join("t");
    for i in 0..20 {
        std::fs::create_dir_all(root.join(format!("d{}", i % 4))).unwrap();
        std::fs::write(root.join(format!("d{}/f{i}", i % 4)), vec![1u8; i * 100]).unwrap();
    }
    let r = root.to_str().unwrap();
    let scan = ["scan", r, "--bytes", "--no-history"];
    let local = common::ok(common::dfdu(&scan));
    let body = |s: &str| {
        s.lines()
            .filter(|l| !l.contains(" items, "))
            .collect::<Vec<_>>()
            .join("\n")
    };

    // auto never sends unasked when nobody can be asked.
    let refused = run(&home, &scan, "auto");
    assert!(!refused.status.success());
    let err = String::from_utf8_lossy(&refused.stderr);
    assert!(err.contains("--remote-agent upload"), "{err}");

    let first = run(&home, &scan, "upload");
    let err = String::from_utf8_lossy(&first.stderr).into_owned();
    assert!(err.contains("sending dfdu agent"), "{err}");
    assert_eq!(body(&common::ok(first)), body(&local));

    // Cached now: auto uses it without asking, and nothing is sent.
    let second = run(&home, &scan, "auto");
    let err = String::from_utf8_lossy(&second.stderr).into_owned();
    assert!(!err.contains("sending"), "{err}");
    assert_eq!(body(&common::ok(second)), body(&local));
}

/// `dfdu args` with its stdin and stderr on a pseudo-terminal, answering `answer` to the
/// question it asks there; its exit status and everything it wrote to the terminal.
fn on_terminal(home: &Path, args: &[&str], answer: &str) -> (bool, String) {
    use rustix::fs::{Mode, OFlags};
    use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
    use std::io::{Read, Write};
    use std::sync::{Arc, Mutex};

    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
    grantpt(&master).unwrap();
    unlockpt(&master).unwrap();
    let name = ptsname(&master, Vec::new()).unwrap();
    let slave = rustix::fs::open(
        name.as_c_str(),
        OFlags::RDWR | OFlags::NOCTTY,
        Mode::empty(),
    )
    .unwrap();
    let mut child = cmd(home, args, Some("auto"))
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::null())
        .stderr(Stdio::from(slave.try_clone().unwrap()))
        .spawn()
        .unwrap();
    drop(slave);
    let out = Arc::new(Mutex::new(Vec::new()));
    let mut reader = std::fs::File::from(master.try_clone().unwrap());
    let o = out.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            o.lock().unwrap().extend_from_slice(&buf[..n]);
        }
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !String::from_utf8_lossy(&out.lock().unwrap()).contains("[y/N]") {
        assert!(std::time::Instant::now() < deadline, "no question asked");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    std::fs::File::from(master.try_clone().unwrap())
        .write_all(format!("{answer}\n").as_bytes())
        .unwrap();
    let ok = child.wait().unwrap().success();
    // The reader ends at the slave's last close, which was the child's.
    std::thread::sleep(std::time::Duration::from_millis(100));
    let text = String::from_utf8_lossy(&out.lock().unwrap()).into_owned();
    (ok, text)
}

#[test]
fn asks_on_the_terminal_before_sending() {
    let tmp = tempfile::tempdir().unwrap();
    let home = dunce::canonicalize(tmp.path()).unwrap();
    std::fs::create_dir_all(home.join("t/d")).unwrap();
    let r = home.join("t");
    let scan = ["scan", r.to_str().unwrap(), "--no-history"];

    let (ok, text) = on_terminal(&home, &scan, "n");
    assert!(!ok, "{text}");
    assert!(
        text.contains("has no dfdu") && text.contains("Send one ("),
        "{text}"
    );
    assert!(text.contains("--remote-agent upload"), "{text}");
    assert!(!text.contains("sending"), "{text}");

    let (ok, text) = on_terminal(&home, &scan, "y");
    assert!(ok, "{text}");
    assert!(text.contains("sending dfdu agent"), "{text}");
}

#[test]
fn a_failed_ssh_is_reported_as_such() {
    let tmp = tempfile::tempdir().unwrap();
    let home = dunce::canonicalize(tmp.path()).unwrap();
    let ssh = home.join("ssh");
    std::fs::write(
        &ssh,
        "#!/bin/sh\necho \"ssh: connect: Connection refused\" >&2\nexit 255\n",
    )
    .unwrap();
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
    let o = run(&home, &["scan", "/", "--no-history"], "auto");
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("Connection refused"), "{err}");
    assert!(err.contains("exited with status 255"), "{err}");
    assert!(!err.contains("is its shell"), "{err}");
}

#[test]
fn the_mode_comes_from_the_environment() {
    let tmp = tempfile::tempdir().unwrap();
    let home = dunce::canonicalize(tmp.path()).unwrap();
    let mut c = common::bin_in(&common::shared_state());
    let o = c
        .args(["scan", "/", "--no-history", "--remote", "elsewhere"])
        .env("DFDU_REMOTE_AGENT", "sometimes")
        .env("HOME", &home)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("DFDU_REMOTE_AGENT=sometimes"), "{err}");

    // upload from the environment: no question, no refusal.
    let r = home.join("t");
    std::fs::create_dir_all(&r).unwrap();
    let o = cmd(&home, &["scan", r.to_str().unwrap(), "--no-history"], None)
        .env("DFDU_REMOTE_AGENT", "upload")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}

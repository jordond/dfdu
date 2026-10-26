//! A panic on the UI thread restores the terminal (raw mode off, alternate screen left, cursor
//! shown) and then writes a crash report with the panic, the engine's counters and its last
//! messages (`docs/data-pipeline.md` §8).
//!
//! Runs the real `dfdu` binary on a pseudo-terminal. The environment variable
//! `DFDU_TEST_UI_PANIC` (hidden; read by the event loop in `dfdu_tui`) makes the browser panic on
//! the first key press.

#![cfg(unix)]

use std::fs::{self, File};
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rustix::fs::{Mode, OFlags};
use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
use rustix::termios::{LocalModes, Winsize, tcgetattr, tcsetwinsize};

const ENTER_ALT: &[u8] = b"\x1b[?1049h";
const LEAVE_ALT: &[u8] = b"\x1b[?1049l";
const SHOW_CURSOR: &[u8] = b"\x1b[?25h";

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn wait_until(what: &str, timeout: Duration, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn ui_panic_restores_terminal_and_writes_crash_file() {
    let tmp = tempfile::tempdir().unwrap();
    let tree = tmp.path().join("tree");
    for d in 0..20 {
        let dir = tree.join(format!("d{d}"));
        fs::create_dir_all(&dir).unwrap();
        for f in 0..20 {
            fs::write(dir.join(format!("f{f}")), vec![0u8; 100 * f]).unwrap();
        }
    }
    let state = tmp.path().join("state");

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
    let size = Winsize {
        ws_row: 30,
        ws_col: 100,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    tcsetwinsize(&master, size).unwrap();
    let cooked = tcgetattr(&slave).unwrap().local_modes;
    assert!(cooked.contains(LocalModes::ICANON | LocalModes::ECHO));

    let mut child = Command::new(env!("CARGO_BIN_EXE_dfdu"))
        .arg(&tree)
        .env("DFDU_STATE_DIR", &state)
        .env("DFDU_TEST_UI_PANIC", "1")
        .env("TERM", "xterm-256color")
        .env("RUST_BACKTRACE", "0")
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave.try_clone().unwrap()))
        .spawn()
        .unwrap();

    // Everything the child writes to the terminal. The reader ends when the slave side closes.
    let out = Arc::new(Mutex::new(Vec::new()));
    let mut reader = File::from(master.try_clone().unwrap());
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

    // The browser is up: alternate screen entered (after raw mode was set) and a frame drawn.
    wait_until("the first frame", Duration::from_secs(20), || {
        let o = out.lock().unwrap();
        find(&o, ENTER_ALT).is_some_and(|i| find(&o[i..], b"d19").is_some())
    });
    let raw = tcgetattr(&slave).unwrap().local_modes;
    assert!(
        !raw.intersects(LocalModes::ICANON | LocalModes::ECHO),
        "raw mode during the run: {raw:?}"
    );
    let drawn = out.lock().unwrap().len();

    File::from(master.try_clone().unwrap())
        .write_all(b"x")
        .unwrap();
    let mut status = None;
    wait_until("dfdu to exit", Duration::from_secs(20), || {
        status = child.try_wait().unwrap();
        status.is_some()
    });
    assert!(!status.unwrap().success());

    let restored = tcgetattr(&slave).unwrap().local_modes;
    assert!(
        restored.contains(LocalModes::ICANON | LocalModes::ECHO),
        "raw mode after the panic: {restored:?}"
    );
    wait_until("the crash report notice", Duration::from_secs(5), || {
        find(&out.lock().unwrap(), b"crash report written").is_some()
    });
    let o = out.lock().unwrap()[drawn..].to_vec();
    let text = String::from_utf8_lossy(&o);
    let leave = find(&o, LEAVE_ALT).unwrap_or_else(|| panic!("alternate screen left: {text:?}"));
    let show = find(&o, SHOW_CURSOR).unwrap_or_else(|| panic!("cursor shown: {text:?}"));
    let message = find(&o, b"injected UI panic").unwrap_or_else(|| panic!("message: {text:?}"));
    let notice = find(&o, b"crash report written").unwrap();
    // The terminal is restored before anything is printed.
    assert!(
        leave < message && show < message && leave < notice,
        "{text:?}"
    );

    let reports: Vec<_> = fs::read_dir(state.join("crash"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(reports.len(), 1, "{reports:?}");
    let report = fs::read_to_string(&reports[0]).unwrap();
    for want in [
        "thread: main",
        "injected UI panic",
        "backtrace:",
        "stats: Stats {",
        "messages and commands, oldest first:",
        "Command(\"Scan\")",
        "Chunk {",
        "Close {",
    ] {
        assert!(report.contains(want), "{want:?} missing from:\n{report}");
    }
}

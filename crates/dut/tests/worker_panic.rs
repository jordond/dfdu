//! A panic in a scan worker is caught by the engine and recorded as one `Panicked` directory
//! (`docs/data-pipeline.md` §8). Headless `dut scan` finishes the scan, reports that directory
//! like any other unreadable one (exit status 3), and writes no crash report: the panic hook
//! leaves panics the engine catches alone.
//!
//! The environment variable `DUT_TEST_WORKER_PANIC` (hidden; read by `dut scan`) makes the scan
//! panic when it opens the first directory below the root.

use std::fs;
use std::process::Command;

#[test]
fn caught_worker_panic_is_an_error_entry_without_a_crash_report() {
    let tmp = tempfile::tempdir().unwrap();
    let tree = tmp.path().join("tree");
    for d in 0..5 {
        let dir = tree.join(format!("d{d}"));
        fs::create_dir_all(&dir).unwrap();
        for f in 0..5 {
            fs::write(dir.join(format!("f{f}")), vec![0u8; 100 * f]).unwrap();
        }
    }
    let state = tmp.path().join("state");

    let run = |inject: bool| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_dut"));
        cmd.args(["scan", "--errors", "--threads", "2"])
            .arg(&tree)
            .env("DUT_STATE_DIR", &state)
            .env("RUST_BACKTRACE", "0");
        if inject {
            cmd.env("DUT_TEST_WORKER_PANIC", "1");
        }
        cmd.output().unwrap()
    };

    // Control: without the panic the scan is clean.
    let clean = run(false);
    assert_eq!(clean.status.code(), Some(0), "{clean:?}");

    let out = run(true);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(3), "{stdout}\n{stderr}");
    // One directory is not read; the scan did not stop at it.
    let listed = stdout
        .lines()
        .skip_while(|l| *l != "Not read:")
        .skip(1)
        .filter(|l| l.starts_with("  "))
        .count();
    assert_eq!(listed, 1, "{stdout}\n{stderr}");
    assert!(stderr.contains("1 error"), "{stderr}");
    assert!(stderr.contains("not read: "), "{stderr}");
    // Neither a crash report nor the panic message.
    assert!(!stderr.contains("crash report"), "{stderr}");
    assert!(!stderr.contains("injected worker panic"), "{stderr}");
    assert!(!state.join("crash").exists(), "crash report written");
}

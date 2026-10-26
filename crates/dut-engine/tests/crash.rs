//! A panic in the applier thread is reported, written to a crash file, and does not leave
//! callers waiting forever (`docs/data-pipeline.md` §8).

use std::time::Duration;

use dut_engine::{Command, Engine, EngineOptions};
use dut_scan::{JobOptions, PortableBackend};

#[test]
fn applier_panic_is_fatal_and_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("crash");
    let e = Engine::start(
        PortableBackend,
        EngineOptions {
            crash_dir: Some(dir.clone()),
            workers: 2,
            ..Default::default()
        },
    );
    e.send(Command::Scan {
        path: tmp.path().to_path_buf(),
        opts: JobOptions::default(),
    });
    e.send(Command::InjectPanic);
    // wait_idle must return (false) instead of waiting for a job that will never finish.
    assert!(!e.wait_idle(Some(Duration::from_secs(10))) || e.read(|m| m.status.fatal.is_some()));
    let fatal = e.read(|m| m.status.fatal.clone()).expect("fatal is set");
    assert!(fatal.contains("injected applier panic"), "{fatal}");
    let reports: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
    assert_eq!(reports.len(), 1);
    let text = std::fs::read_to_string(reports[0].as_ref().unwrap().path()).unwrap();
    assert!(text.contains("injected applier panic") && text.contains("backtrace"));
    e.shutdown(Duration::from_millis(500));
}

//! The count of scanner messages sent to the applier, against the applier's count of messages
//! received.

use std::fs;
use std::time::{Duration, Instant};

use dfdu_engine::{Command, Engine, EngineOptions};
use dfdu_scan::{JobOptions, PortableBackend};

#[test]
fn sent_counter_counts_messages() {
    let tmp = tempfile::tempdir_in(dunce::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
    for d in 0..40 {
        let dir = tmp.path().join(format!("d{d}/a/b"));
        fs::create_dir_all(&dir).unwrap();
        for f in 0..50 {
            fs::write(dir.join(format!("f{f}")), [0u8; 10]).unwrap();
        }
    }
    let engine = Engine::start(
        PortableBackend::default(),
        EngineOptions {
            workers: 4,
            journal_dir: None,
            crash_dir: None,
            measure_workers: 0,
            ..Default::default()
        },
    );
    engine.send(Command::Scan {
        path: tmp.path().to_path_buf(),
        opts: JobOptions::default(),
    });
    let mut samples = 0;
    while engine.read(|m| m.status.busy() || m.status.commands_applied == 0) {
        // Under the lock the applier's count cannot move, and every message it counted was
        // counted as sent first.
        let (received, sent) = engine.read(|m| (m.status.stats.messages, engine.sent_messages()));
        assert!(sent >= received, "{sent} sent, {received} received");
        samples += 1;
        std::thread::sleep(Duration::from_micros(200));
    }
    assert!(engine.wait_idle(Some(Duration::from_secs(30))), "scan");
    // Idle does not wait for a message still in the channel (the watchdog's late
    // `Quiescent`): wait until the applier has taken it.
    let settle = Instant::now();
    let (received, sent) = loop {
        let (received, sent) = engine.read(|m| (m.status.stats.messages, engine.sent_messages()));
        if sent == received || settle.elapsed() >= Duration::from_secs(5) {
            break (received, sent);
        }
        std::thread::sleep(Duration::from_millis(1));
    };
    assert!(received > 0);
    assert_eq!(sent, received, "after {samples} samples during the scan");
    engine.shutdown(Duration::from_millis(200));
}

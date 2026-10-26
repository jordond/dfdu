//! Headless `dfdu scan` on a hung mount (ROADMAP M2 acceptance, `docs/data-pipeline.md`
//! §10.4): a FUSE filesystem (`dfdu-hangfs`) mounted as a network filesystem below the scanned
//! directory, two of its directories never answering their listing and one file never
//! answering its stat. `dfdu scan --network --stall-abandon` reports within a bound, with the
//! three directories listed as not responding, and exits with code 3.
//!
//! The process can exit only once the stuck calls return: the kernel waits out a FUSE request
//! the daemon has already read, even for a killed process. So the test waits for the report,
//! then answers the hung requests (as a daemon that recovers, or the connection being aborted,
//! would) and checks that dfdu then exits at once.
//!
//! Needs `/dev/fuse` and root (or `fusermount3`); skipped with a message otherwise, unless
//! `DFDU_FUSE_REQUIRED` is set (CI).
#![cfg(target_os = "linux")]

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dfdu_hangfs::{Mounted, Options, Spec};

#[test]
fn headless_scan_abandons_hung_directories() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let at = root.join("mnt");
    std::fs::create_dir(&at).unwrap();
    let (spec, hung) = Spec::hanging(10);
    let opts = Options {
        subtype: Some("sshfs".into()),
        delay: Duration::ZERO,
    };
    let fs = match Mounted::mount(spec, &at, opts) {
        Ok(m) => m,
        Err(e) if std::env::var_os("DFDU_FUSE_REQUIRED").is_none() => {
            eprintln!("skipped: cannot mount a FUSE filesystem ({e}); needs /dev/fuse and root");
            return;
        }
        Err(e) => panic!("cannot mount a FUSE filesystem: {e}"),
    };

    let t0 = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_dfdu"))
        .args(["scan", "--network", "--errors", "--depth", "2"])
        .args(["--stall-after", "0.3", "--stall-abandon", "0.5"])
        .arg(&root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let collect = |mut r: Box<dyn Read + Send>| {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let b = buf.clone();
        std::thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            while let Ok(n) = r.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                b.lock().unwrap().extend_from_slice(&chunk[..n]);
            }
        });
        buf
    };
    let stdout = collect(Box::new(child.stdout.take().unwrap()));
    let stderr = collect(Box::new(child.stderr.take().unwrap()));
    let text = |b: &Arc<Mutex<Vec<u8>>>| String::from_utf8_lossy(&b.lock().unwrap()).into_owned();
    // Stall after 0.3 s, abandon 0.5 s later: the report (its last line is the footer on
    // stderr) comes well within 20 s, even on a loaded machine.
    let bound = Duration::from_secs(20);
    while !text(&stderr).contains(" items, ") {
        if t0.elapsed() > bound {
            fs.release();
            let _ = child.kill();
            panic!(
                "no report within {bound:?}\n{}\n{}",
                text(&stdout),
                text(&stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let reported = t0.elapsed();
    // The scan is over; the three threads stuck in the kernel keep the process from exiting.
    std::thread::sleep(Duration::from_millis(700));
    let stuck = child.try_wait().unwrap().is_none();
    fs.release();
    let released = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if released.elapsed() > Duration::from_secs(10) {
            let _ = child.kill();
            panic!("dfdu did not exit after the hung requests were answered");
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let exited = released.elapsed();
    let (stdout, stderr) = (text(&stdout), text(&stderr));
    assert_eq!(
        status.code(),
        Some(3),
        "partial scan exit code\n{stdout}\n{stderr}"
    );
    for h in &hung {
        let line = format!("{}/: not responding: skipped", at.join(h).display());
        assert!(
            stdout.contains(&line),
            "{line:?} not in\n{stdout}\n{stderr}"
        );
    }
    eprintln!(
        "dfdu scan with 3 hung calls: report after {reported:?}; still running 0.7 s later: \
         {stuck}; exited {exited:?} after the requests were answered\n{stdout}{stderr}"
    );
    drop(fs);
    assert_eq!(std::fs::read_dir(&at).unwrap().count(), 0, "still mounted");
}

//! `--remote-agent auto` asking on a terminal before it sends the agent.

use std::path::Path;
use std::process::Stdio;

use super::cmd;

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
    let reading = std::thread::spawn(move || {
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
    // The reader ends at the slave's last close, which was the child's, once it has read
    // everything written before.
    reading.join().unwrap();
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

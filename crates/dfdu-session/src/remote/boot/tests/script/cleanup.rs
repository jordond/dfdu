//! What the script leaves behind: nothing of a failed upload, and no agent left unused.

use super::*;

#[test]
fn script_keeps_nothing_it_did_not_install() {
    let host = Host::new("/bin/sh");
    // Declined: no key directory, no temporary file.
    let (mut child, w, _r, _) = host.until_send("k1");
    drop(w);
    child.wait().unwrap();
    assert_eq!(host.cache_files(), Vec::<String>::new());

    // Corrupted.
    let (mut child, mut w, mut r, _) = host.until_send("k1");
    writeln!(w, "k1 3 {}", "0".repeat(64)).unwrap();
    w.write_all(b"abc").unwrap();
    w.flush().unwrap();
    assert_eq!(line(&mut r).unwrap(), "BAD");
    child.wait().unwrap();
    assert_eq!(host.cache_files(), Vec::<String>::new());

    // Cut short.
    let (mut child, mut w, mut r, _) = host.until_send("k1");
    writeln!(w, "k1 100 {}", "0".repeat(64)).unwrap();
    w.write_all(b"0123456789").unwrap();
    drop(w);
    assert_eq!(line(&mut r).unwrap(), "ERR received 10 of 100 bytes");
    child.wait().unwrap();
    assert_eq!(host.cache_files(), Vec::<String>::new());

    // A key that could name another directory.
    let (mut child, mut w, mut r, _) = host.until_send("k1");
    writeln!(w, "../k1 3 {}", "0".repeat(64)).unwrap();
    assert_eq!(line(&mut r).unwrap(), "ERR bad key");
    child.wait().unwrap();
}

#[test]
fn script_prunes_what_was_left_unused() {
    let host = Host::new("/bin/sh");
    let base = host.path(".cache/dfdu/agent");
    let old = base.join("old");
    std::fs::create_dir_all(&old).unwrap();
    std::fs::write(base.join(".dfdu.stray"), b"partial").unwrap();
    std::fs::write(base.join(".dfdu.fresh"), b"in flight").unwrap();
    let touch = |p: &std::path::Path| {
        let status = std::process::Command::new("touch")
            .args(["-t", "202001010000"])
            .arg(p)
            .status()
            .unwrap();
        assert!(status.success());
    };
    touch(&old);
    touch(&base.join(".dfdu.stray"));

    let v = dfdu_engine::wire::VERSION;
    assert_eq!(host.boot("upload", &agent(v, v)).0.unwrap(), How::Uploaded);
    assert_eq!(host.cache_files(), [".dfdu.fresh", "k1", "k1/dfdu"]);
}

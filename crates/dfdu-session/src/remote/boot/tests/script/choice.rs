//! Which agent the script runs: one it was sent, one it keeps, or an installed dfdu; and
//! where it keeps them.

use super::*;

#[test]
fn script_uploads_then_reuses() {
    let v = dfdu_engine::wire::VERSION;
    for shell in shells() {
        let host = Host::new(shell);
        let bin = host.path(".cache/dfdu/agent/k1/dfdu");
        let (how, out) = host.boot("upload", &agent(v, v));
        assert_eq!(how.unwrap(), How::Uploaded, "{shell}");
        assert_eq!(out, format!("agent at {}\n", bin.display()), "{shell}");
        let (how, out) = host.boot("upload", &agent(v, v));
        assert_eq!(how.unwrap(), How::Cached, "{shell}");
        assert_eq!(out, format!("agent at {}\n", bin.display()), "{shell}");
        // Any key offered will do.
        let (how, _) = host.boot_keys("upload", &["k0", "k1"], &agent(v, v));
        assert_eq!(how.unwrap(), How::Cached, "{shell}");

        // One that does not run (or speaks another protocol) is replaced.
        std::fs::write(&bin, b"garbage").unwrap();
        assert_eq!(host.boot("upload", &agent(v, v)).0.unwrap(), How::Uploaded);
        std::fs::write(&bin, agent(v + 1, v + 1)).unwrap();
        assert_eq!(host.boot("upload", &agent(v, v)).0.unwrap(), How::Uploaded);
        assert_eq!(host.boot("upload", &agent(v, v)).0.unwrap(), How::Cached);
    }
}

#[test]
fn script_prefers_an_installed_dfdu_that_speaks_ours() {
    let v = dfdu_engine::wire::VERSION;
    let host = Host::new("/bin/sh");
    let old = String::from_utf8(agent(v - 1, v - 1)).unwrap();
    host.script("bin/dfdu", old.trim_start_matches("#!/bin/sh\n"));
    let (how, _) = host.boot("auto", &agent(v, v));
    assert_eq!(how.unwrap(), How::Uploaded, "an old dfdu is passed over");

    let new = String::from_utf8(agent(v - 1, v + 1)).unwrap();
    host.script("bin/dfdu", new.trim_start_matches("#!/bin/sh\n"));
    let (how, out) = host.boot("auto", &agent(v, v));
    assert_eq!(how.unwrap(), How::Installed);
    assert!(out.starts_with("agent at "), "{out}");
    // upload mode never runs the installed one.
    assert_eq!(host.boot("upload", &agent(v, v)).0.unwrap(), How::Cached);
}

#[test]
fn script_skips_unwritable_and_symlinked_dirs() {
    let v = dfdu_engine::wire::VERSION;
    let mut host = Host::new("/bin/sh");
    // A file where the cache directory should be: mkdir fails there, even as root.
    std::fs::write(host.path("cache"), b"").unwrap();
    host.env
        .push(("XDG_CACHE_HOME", host.path("cache").display().to_string()));
    // A symlink where the next one should be, to a directory of ours: not used either.
    std::fs::create_dir_all(host.path("run")).unwrap();
    std::fs::create_dir_all(host.path("elsewhere")).unwrap();
    std::os::unix::fs::symlink(host.path("elsewhere"), host.path("run/dfdu-agent")).unwrap();
    host.env
        .push(("XDG_RUNTIME_DIR", host.path("run").display().to_string()));
    let (how, _) = host.boot("upload", &agent(v, v));
    assert!(
        std::fs::read_dir(host.path("elsewhere"))
            .unwrap()
            .next()
            .is_none()
    );
    // The last candidate, /dev/shm, exists on Linux only, and is often mounted noexec.
    match how {
        Ok(how) => assert_eq!(how, How::Uploaded),
        Err(e) => {
            let e = e.to_string();
            assert!(
                e.contains("no directory of ours") || e.contains("does not run there"),
                "{e}"
            );
        }
    }
    let uid = std::process::Command::new("id").arg("-u").output().unwrap();
    let uid = String::from_utf8(uid.stdout).unwrap();
    let _ = std::fs::remove_dir_all(format!("/dev/shm/dfdu-agent-{}", uid.trim()));

    // An unwritable first directory falls back to the runtime one.
    let mut host = Host::new("/bin/sh");
    std::fs::write(host.path("cache"), b"").unwrap();
    host.env
        .push(("XDG_CACHE_HOME", host.path("cache").display().to_string()));
    host.env
        .push(("XDG_RUNTIME_DIR", host.path("run").display().to_string()));
    std::fs::create_dir(host.path("run")).unwrap();
    let (how, out) = host.boot("upload", &agent(v, v));
    assert_eq!(how.unwrap(), How::Uploaded);
    let bin = host.path("run/dfdu-agent/k1/dfdu");
    assert_eq!(out, format!("agent at {}\n", bin.display()));
}

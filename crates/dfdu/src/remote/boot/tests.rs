#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

/// A supply that records what it was asked, offers `k1` and `k2`, and sends `abc` as `k1`.
#[derive(Default)]
struct Fake {
    allow: bool,
    asked: Vec<String>,
}

impl Supply for Fake {
    fn keys(&mut self, target: &'static str) -> io::Result<Vec<String>> {
        self.asked.push(format!("keys {target}"));
        Ok(vec!["k1".into(), "k2".into()])
    }
    fn allow(&mut self, _: &'static str, dir: &str) -> io::Result<()> {
        self.asked.push(format!("allow {dir}"));
        if self.allow {
            Ok(())
        } else {
            Err(io::Error::other("declined"))
        }
    }
    fn send(&mut self, _: &'static str) -> io::Result<(String, Vec<u8>)> {
        Ok(("k1".into(), b"abc".to_vec()))
    }
}

fn exchange_with(
    script_out: &str,
    allow: bool,
    w: &mut impl Write,
) -> (io::Result<How>, Vec<String>) {
    let mut r = io::Cursor::new(script_out.as_bytes().to_vec());
    let mut s = Fake {
        allow,
        ..Fake::default()
    };
    let how = run(&mut r, w, &mut s);
    (how, s.asked)
}

fn exchange(script_out: &str, allow: bool) -> (io::Result<How>, String, Vec<String>) {
    let mut w = Vec::new();
    let (how, asked) = exchange_with(script_out, allow, &mut w);
    (how, String::from_utf8(w).unwrap(), asked)
}

#[test]
fn marker_after_noise_then_installed() {
    let (how, sent, _) = exchange("welcome\nmotd\nDFDU-BOOT 1 Linux x86_64\nINSTALLED\n", true);
    assert_eq!(how.unwrap(), How::Installed);
    assert_eq!(sent, "", "nothing may reach an installed agent's stdin");
    // An rc file's output without a newline lands before the marker.
    let (how, _, _) = exchange(
        "\x1b]0;title\x07DFDU-BOOT 1 Linux x86_64\nINSTALLED\n",
        true,
    );
    assert_eq!(how.unwrap(), How::Installed);
}

#[test]
fn too_much_noise_fails() {
    let noise = "x\n".repeat(NOISE_LINES + 1) + "DFDU-BOOT 1 Linux x86_64\n";
    let err = exchange(&noise, true).0.unwrap_err();
    assert!(err.to_string().contains("no bootstrap marker"), "{err}");
    let long = "y".repeat(20 << 10) + "\nDFDU-BOOT 1 Linux x86_64\n";
    assert!(exchange(&long, true).0.is_err());
}

#[test]
fn closed_before_the_marker_is_an_eof() {
    let err = exchange("", true).0.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    let err = exchange("motd\n", true).0.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn cached_agent_after_keys() {
    let (how, sent, asked) = exchange("DFDU-BOOT 1 Linux aarch64\nKEY?\nHAVE\n", true);
    assert_eq!(how.unwrap(), How::Cached);
    assert_eq!(sent, "KEY k1 k2\n");
    assert_eq!(asked, ["keys aarch64-unknown-linux-musl"]);
}

#[test]
fn sends_key_size_hash_and_bytes() {
    let (how, sent, asked) = exchange(
        "DFDU-BOOT 1 Darwin arm64\nKEY?\nSEND /h/.cache/dfdu/agent\nOK\n",
        true,
    );
    assert_eq!(how.unwrap(), How::Uploaded);
    assert_eq!(
        sent,
        "KEY k1 k2\nk1 3 ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad\nabc"
    );
    assert_eq!(asked[1], "allow /h/.cache/dfdu/agent");
}

#[test]
fn declined_sends_nothing() {
    let (how, sent, _) = exchange("DFDU-BOOT 1 Linux x86_64\nKEY?\nSEND /d\n", false);
    assert_eq!(how.unwrap_err().to_string(), "declined");
    assert_eq!(sent, "KEY k1 k2\n");
}

#[test]
fn failures_name_the_cause() {
    let err = |out: &str| exchange(out, true).0.unwrap_err().to_string();
    assert_eq!(
        err("DFDU-BOOT 1 Linux x86_64\nKEY?\nSEND /d\nBAD\n"),
        "the agent arrived corrupted"
    );
    assert_eq!(
        err("DFDU-BOOT 1 Linux x86_64\nKEY?\nERR no directory to keep the agent in\n"),
        "no directory to keep the agent in"
    );
    assert!(err("DFDU-BOOT 1 Linux x86_64\nKEY?\n").contains("closed the connection"));
    assert!(err("DFDU-BOOT 1 FreeBSD amd64\nKEY?\n").contains("no dfdu build for FreeBSD amd64"));
    assert!(err("DFDU-BOOT 1 Linux x86_64\nwhat\n").contains("unexpected reply"));
    // Text from the host cannot drive the terminal.
    assert_eq!(
        err("DFDU-BOOT 1 Linux x86_64\nKEY?\nERR evil \x1b[2J\n"),
        "evil ?[2J"
    );
}

#[test]
fn a_script_that_stops_reading_says_why() {
    /// Takes this many lines, then fails like a pipe whose reader has gone.
    struct Closed(usize);
    impl Write for Closed {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            if self.0 == 0 {
                return Err(io::ErrorKind::BrokenPipe.into());
            }
            self.0 -= b.iter().filter(|&&c| c == b'\n').count().min(self.0);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let out = "DFDU-BOOT 1 Linux x86_64\nKEY?\nSEND /d\nERR cannot write in /d (disk full?)\n";
    let err = exchange_with(out, true, &mut Closed(1)).0.unwrap_err();
    assert_eq!(err.to_string(), "cannot write in /d (disk full?)");
    let out = "DFDU-BOOT 1 Linux x86_64\nKEY?\nSEND /d\n";
    let err = exchange_with(out, true, &mut Closed(1)).0.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::BrokenPipe);
}

#[test]
fn script_survives_single_quoting() {
    assert!(
        !SCRIPT.contains('\''),
        "a quote would end the single-quoted script"
    );
    assert!(
        !SCRIPT.contains('\\'),
        "fish unescapes backslashes in single quotes"
    );
    assert!(command(AgentMode::Upload).ends_with(&format!(
        "' dfdu-boot {} upload",
        dfdu_engine::wire::VERSION
    )));
}

/// The script run by `shell` on this machine, as ssh would start it, in a home of its own.
#[cfg(unix)]
struct Host {
    home: tempfile::TempDir,
    shell: &'static str,
    env: Vec<(&'static str, String)>,
}

#[cfg(unix)]
impl Host {
    fn new(shell: &'static str) -> Host {
        Host {
            home: tempfile::tempdir().unwrap(),
            shell,
            env: Vec::new(),
        }
    }

    fn path(&self, p: &str) -> std::path::PathBuf {
        self.home.path().join(p)
    }

    /// Writes an executable script at `p`.
    fn script(&self, p: &str, body: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = self.path(p);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn start(&self, mode: &str) -> std::process::Child {
        use std::process::{Command, Stdio};
        let mut c = Command::new(self.shell);
        c.args(["-c", SCRIPT, "dfdu-boot"])
            .arg(dfdu_engine::wire::VERSION.to_string())
            .arg(mode)
            .env_clear()
            .env("HOME", self.home.path())
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.path("bin").display()),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        for (k, v) in &self.env {
            c.env(k, v);
        }
        c.spawn().unwrap()
    }

    /// Runs the bootstrap offering `keys` and sending `agent` as the first; how it went and
    /// what the agent printed.
    fn boot_keys(&self, mode: &str, keys: &[&str], agent: &[u8]) -> (io::Result<How>, String) {
        struct Send<'a>(Vec<String>, &'a [u8]);
        impl Supply for Send<'_> {
            fn keys(&mut self, _: &'static str) -> io::Result<Vec<String>> {
                Ok(self.0.clone())
            }
            fn allow(&mut self, _: &'static str, _: &str) -> io::Result<()> {
                Ok(())
            }
            fn send(&mut self, _: &'static str) -> io::Result<(String, Vec<u8>)> {
                Ok((self.0[0].clone(), self.1.to_vec()))
            }
        }
        let mut child = self.start(mode);
        let mut r = io::BufReader::new(child.stdout.take().unwrap());
        let mut w = child.stdin.take().unwrap();
        let keys = keys.iter().map(|k| (*k).to_string()).collect();
        let how = run(&mut r, &mut w, &mut Send(keys, agent));
        drop(w);
        let mut rest = String::new();
        r.read_to_string(&mut rest).unwrap();
        child.wait().unwrap();
        (how, rest)
    }

    fn boot(&self, mode: &str, agent: &[u8]) -> (io::Result<How>, String) {
        self.boot_keys(mode, &["k1"], agent)
    }

    /// Runs the script up to `SEND`; its stdin, its stdout, and the dir it named.
    fn until_send(
        &self,
        key: &str,
    ) -> (
        std::process::Child,
        std::process::ChildStdin,
        io::BufReader<std::process::ChildStdout>,
        String,
    ) {
        let mut child = self.start("upload");
        let mut r = io::BufReader::new(child.stdout.take().unwrap());
        let mut w = child.stdin.take().unwrap();
        marker(&mut r).unwrap();
        assert_eq!(line(&mut r).unwrap(), "KEY?");
        writeln!(w, "KEY {key}").unwrap();
        let send = line(&mut r).unwrap();
        let dir = send.strip_prefix("SEND ").expect(&send).to_string();
        (child, w, r, dir)
    }

    /// The files below the agent cache, by path relative to it.
    fn cache_files(&self) -> Vec<String> {
        fn walk(d: &std::path::Path, base: &std::path::Path, out: &mut Vec<String>) {
            for e in std::fs::read_dir(d).into_iter().flatten() {
                let p = e.unwrap().path();
                out.push(p.strip_prefix(base).unwrap().display().to_string());
                walk(&p, base, out);
            }
        }
        let base = self.path(".cache/dfdu/agent");
        let mut out = Vec::new();
        walk(&base, &base, &mut out);
        out.sort();
        out
    }
}

/// An agent that speaks protocol `lo`-`hi` and, run as `agent`, says where it is.
#[cfg(unix)]
fn agent(lo: u32, hi: u32) -> Vec<u8> {
    format!(
        "#!/bin/sh\nif [ \"$2\" = --protocols ]; then echo {lo} {hi}; else echo \"agent at $0\"; fi\n"
    )
    .into_bytes()
}

#[cfg(unix)]
fn shells() -> Vec<&'static str> {
    ["/bin/sh", "/bin/dash", "/usr/bin/dash", "/bin/bash"]
        .into_iter()
        .filter(|s| std::path::Path::new(s).exists())
        .collect()
}

#[cfg(unix)]
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

#[cfg(unix)]
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

#[cfg(unix)]
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

#[cfg(unix)]
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

#[cfg(unix)]
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

#[test]
fn upload_decision() {
    let never = || -> io::Result<String> { panic!("must not ask") };
    assert!(decide(AgentMode::Upload, false, never).is_ok());
    assert!(decide(AgentMode::Upload, true, never).is_ok());
    let refused = decide(AgentMode::Auto, false, never).unwrap_err();
    assert!(refused.contains("--remote-agent upload"));
    assert!(decide(AgentMode::Auto, true, || Ok("y\n".into())).is_ok());
    assert!(decide(AgentMode::Auto, true, || Ok("\n".into())).is_err());
    assert!(decide(AgentMode::Auto, true, || Ok("n".into())).is_err());
    assert!(decide(AgentMode::Auto, true, || Err(io::Error::other("no tty"))).is_err());
}

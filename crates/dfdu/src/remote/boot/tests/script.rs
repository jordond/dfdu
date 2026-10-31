//! The script itself, run by the shells on this machine as ssh would start it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{self, Read, Write};

use super::super::*;

mod choice;
mod cleanup;

/// The script run by `shell` on this machine, as ssh would start it, in a home of its own.
struct Host {
    home: tempfile::TempDir,
    shell: &'static str,
    env: Vec<(&'static str, String)>,
}

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
fn agent(lo: u32, hi: u32) -> Vec<u8> {
    format!(
        "#!/bin/sh\nif [ \"$2\" = --protocols ]; then echo {lo} {hi}; else echo \"agent at $0\"; fi\n"
    )
    .into_bytes()
}

fn shells() -> Vec<&'static str> {
    ["/bin/sh", "/bin/dash", "/usr/bin/dash", "/bin/bash"]
        .into_iter()
        .filter(|s| std::path::Path::new(s).exists())
        .collect()
}

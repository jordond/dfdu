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

#[test]
fn upload_decision() {
    let never = || -> io::Result<String> { panic!("must not ask") };
    assert!(decide(AgentMode::Upload, false, never).is_ok());
    assert!(decide(AgentMode::Upload, true, never).is_ok());
    let refused = decide(AgentMode::Auto, false, never).unwrap_err();
    let refused = refused.to_string();
    assert!(refused.contains("--remote-agent upload"), "{refused}");
    assert!(refused.contains(env!("CARGO_PKG_VERSION")), "{refused}");
    assert!(decide(AgentMode::Auto, true, || Ok("y\n".into())).is_ok());
    assert!(decide(AgentMode::Auto, true, || Ok("\n".into())).is_err());
    assert!(decide(AgentMode::Auto, true, || Ok("n".into())).is_err());
    assert!(decide(AgentMode::Auto, true, || Err(io::Error::other("no tty"))).is_err());
}

#[cfg(unix)]
mod script;

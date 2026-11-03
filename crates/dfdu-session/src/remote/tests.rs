#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

fn agents() -> fetch::Agents {
    fetch::Agents {
        agent_dir: None,
        this: None,
        cache: None,
        release_url: "http://127.0.0.1:1".into(),
    }
}

fn ssh(mode: AgentMode) -> Ssh {
    Ssh {
        program: vec!["ssh".into()],
        replaced: None,
        ..Ssh::new("me@host".into(), mode, agents())
    }
}

#[derive(Default)]
struct Quiet {
    notes: Vec<String>,
}

impl Front for Quiet {
    fn allow(&mut self, _: &str, _: &str) -> io::Result<()> {
        Err(io::Error::other("declined"))
    }
    fn note(&mut self, msg: &str) {
        self.notes.push(msg.into());
    }
}

#[test]
fn the_port_and_key_go_before_the_host() {
    let mut s = ssh(AgentMode::Installed);
    s.port = Some(2222);
    s.identity = Some(PathBuf::from("/k/id_ed25519"));
    let (argv, boot) = s.argv();
    let argv: Vec<_> = argv
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        argv,
        [
            "ssh",
            "-p",
            "2222",
            "-i",
            "/k/id_ed25519",
            "-o",
            "IdentitiesOnly=yes",
            "--",
            "me@host",
            "dfdu",
            "agent"
        ]
    );
    assert!(!boot);
}

#[test]
fn auto_runs_the_bootstrap_and_a_replaced_command_does_not() {
    let (argv, boot) = ssh(AgentMode::Auto).argv();
    assert!(boot);
    let last = argv.last().unwrap().to_string_lossy();
    assert!(last.starts_with("sh -c '"), "{last}");
    assert!(last.ends_with(" auto"), "{last}");
    let mut s = ssh(AgentMode::Auto);
    s.replaced = Some(vec!["dfdu".into(), "agent".into()]);
    assert_eq!(s.argv(), (vec!["dfdu".into(), "agent".into()], false));
}

#[cfg(unix)]
mod spawned {
    use super::*;

    /// ssh that runs `script` in sh instead.
    fn fake(script: &str) -> Ssh {
        Ssh {
            program: vec!["sh".into(), "-c".into(), script.into(), "ssh".into()],
            ..ssh(AgentMode::Auto)
        }
    }

    #[test]
    fn a_failed_login_says_what_ssh_said() {
        let s = fake("echo 'me@host: Permission denied (publickey).' >&2; exit 255");
        let err = connect(&s, &mut Quiet::default(), &Cancel::default())
            .err()
            .expect("an error");
        assert!(err.to_string().contains("Permission denied"), "{err}");
    }

    #[test]
    fn ssh_gets_the_environment_given() {
        let mut s = fake("echo \"askpass is $SSH_ASKPASS\" >&2; exit 255");
        s.env = vec![("SSH_ASKPASS".into(), "/x/helper".into())];
        let err = connect(&s, &mut Quiet::default(), &Cancel::default())
            .err()
            .expect("an error");
        assert!(err.to_string().contains("askpass is /x/helper"), "{err}");
    }

    #[test]
    fn cancel_kills_ssh_while_it_waits() {
        let s = fake("exec sleep 30");
        let cancel = Cancel::default();
        let other = cancel.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(200));
            other.cancel();
        });
        let started = std::time::Instant::now();
        let err = connect(&s, &mut Quiet::default(), &cancel)
            .err()
            .expect("an error");
        t.join().unwrap();
        assert_eq!(err.kind(), io::ErrorKind::Interrupted, "{err}");
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
    }

    #[test]
    fn cancelled_before_it_starts_starts_nothing() {
        let cancel = Cancel::default();
        cancel.cancel();
        let err = connect(&fake("exec sleep 30"), &mut Quiet::default(), &cancel)
            .err()
            .expect("an error");
        assert_eq!(err.kind(), io::ErrorKind::Interrupted);
    }
}

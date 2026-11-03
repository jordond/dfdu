use super::*;

type Done = Sender<Result<remote::Opened, String>>;

/// A connection to `me@nas` with no thread behind it: what it gets comes from the test.
fn connecting() -> (Connecting, Done, Cancel) {
    let (done_tx, done) = crossbeam_channel::bounded(1);
    let (_, events) = crossbeam_channel::unbounded();
    let cancel = Cancel::default();
    let c = Connecting::with(target("me@nas", "~"), None, cancel.clone(), done, events);
    (c, done_tx, cancel)
}

const ASKED: &str = "me@nas's password:";

fn answer(rx: &Receiver<Option<Zeroizing<String>>>) -> Option<String> {
    rx.try_recv().ok().flatten().map(|s| s.as_str().to_owned())
}

#[test]
fn a_password_the_same_ssh_asks_for_again_is_forgotten_and_asked() {
    let (mut c, _done, _) = connecting();
    let ctx = egui::Context::default();
    let mut secrets = Secrets::default();
    secrets.keep("ssh:me@nas", Zeroizing::new("old".into()), false);
    let (ask, first) = Ask::new(ASKED, 7);
    c.asked(ask, &mut secrets, &ctx);
    assert_eq!(answer(&first).as_deref(), Some("old"));
    assert!(c.question.is_none());

    let (ask, _second) = Ask::new(ASKED, 7);
    c.asked(ask, &mut secrets, &ctx);
    assert!(matches!(
        c.question,
        Some(Question::Secret { again: true, .. })
    ));
    assert!(secrets.get("ssh:me@nas").is_none());
    assert!(c.rejected.contains("ssh:me@nas"));
}

#[test]
fn another_ssh_of_the_connection_gets_the_password_given() {
    let (mut c, _done, _) = connecting();
    let ctx = egui::Context::default();
    let mut secrets = Secrets::default();
    secrets.keep("ssh:me@nas", Zeroizing::new("pw".into()), false);
    let (ask, _) = Ask::new(ASKED, 7);
    c.asked(ask, &mut secrets, &ctx);
    // A jump host's ssh, then the one behind it.
    let (ask, jumped) = Ask::new(ASKED, 8);
    c.asked(ask, &mut secrets, &ctx);
    assert_eq!(answer(&jumped).as_deref(), Some("pw"));
    assert!(c.question.is_none());
    assert!(secrets.get("ssh:me@nas").is_some());
}

#[test]
fn declining_a_password_stops_connecting() {
    let (mut c, _done, cancel) = connecting();
    let (ask, asked) = Ask::new(ASKED, 7);
    let q = Question::Secret {
        ask,
        kind: askpass::kind(ASKED),
        id: Some("ssh:me@nas".into()),
        input: Zeroizing::default(),
        remember: false,
        again: false,
    };
    c.decide(q, false);
    assert_eq!(answer(&asked), None);
    assert!(cancel.is_cancelled());
}

#[test]
fn a_host_key_is_asked_and_never_answered_from_what_is_kept() {
    let (mut c, _done, _) = connecting();
    let host = "Are you sure you want to continue connecting (yes/no/[fingerprint])?";
    let (ask, asked) = Ask::new(host, 7);
    c.asked(ask, &mut Secrets::default(), &egui::Context::default());
    let Some(q) = c.question.take() else {
        panic!("no question")
    };
    assert!(matches!(q, Question::YesNo { host_key: true, .. }));
    c.decide(q, true);
    assert_eq!(answer(&asked).as_deref(), Some("yes"));
}

#[test]
fn a_connection_that_stopped_or_was_cancelled_says_so() {
    let ctx = egui::Context::default();
    let (mut c, done, _) = connecting();
    drop(done);
    let Some(Outcome::Failed(why)) = c.poll(&mut Secrets::default(), &ctx) else {
        panic!("not failed")
    };
    assert!(why.contains("stopped unexpectedly"), "{why}");
    assert!(c.poll(&mut Secrets::default(), &ctx).is_none());

    let (mut c, done, cancel) = connecting();
    cancel.cancel();
    let _ = done.send(Err("ssh: killed".into()));
    let Some(Outcome::Failed(why)) = c.poll(&mut Secrets::default(), &ctx) else {
        panic!("not failed")
    };
    assert_eq!(why, "");
}

fn target(host: &str, path: &str) -> Target {
    Target {
        host: host.into(),
        port: None,
        identity: None,
        path: path.into(),
    }
}

#[test]
fn recent_targets_read_back_as_written() {
    let recent = vec![
        Target {
            port: Some(2222),
            identity: Some(PathBuf::from("/k/id ed")),
            ..target("me@nas", "~/My Files")
        },
        target("pi", ""),
    ];
    assert_eq!(decode(&encode(&recent)), recent);
    assert!(decode("\nnas\tnot-a-port\t\t~\n\t\t\t~").is_empty());
}

#[test]
fn a_target_a_tab_would_split_is_not_kept() {
    let recent = vec![target("nas", "a\tb"), target("pi", "~")];
    assert_eq!(decode(&encode(&recent)), [target("pi", "~")]);
}

#[test]
fn a_key_cut_short_is_the_one_chosen() {
    let cut = Kind::Passphrase {
        key: "/very/long/pa".into(),
    };
    let whole = Kind::Passphrase {
        key: "/very/long/path/id".into(),
    };
    assert_eq!(
        whole_key(cut.clone(), Some(Path::new("/very/long/path/id"))),
        whole
    );
    assert_eq!(whole_key(cut.clone(), Some(Path::new("/other/id"))), cut);
    assert_eq!(whole_key(cut.clone(), None), cut);
    assert_eq!(
        whole_key(Kind::HostKey, Some(Path::new("/k"))),
        Kind::HostKey
    );
}

#[test]
fn remembered_first_once_and_only_the_latest() {
    let mut recent = Vec::new();
    for n in 0..10 {
        remember(&mut recent, &target(&format!("h{n}"), "~"));
    }
    remember(&mut recent, &target("h7", "~"));
    let hosts: Vec<_> = recent.iter().map(|t| t.host.as_str()).collect();
    assert_eq!(hosts, ["h7", "h9", "h8", "h6", "h5", "h4"]);
}

#[test]
fn a_program_replaced_while_it_runs_is_run_from_its_path() {
    let deleted = PathBuf::from("/usr/bin/dfdu-gui (deleted)");
    assert_eq!(live(deleted.clone(), |_| true), "/usr/bin/dfdu-gui");
    // Nothing there now: the name as it is, for ssh to say it cannot run it.
    assert_eq!(live(deleted.clone(), |_| false), deleted.into_os_string());
    let plain = PathBuf::from("/usr/bin/dfdu-gui");
    assert_eq!(live(plain.clone(), |_| true), plain.into_os_string());
}

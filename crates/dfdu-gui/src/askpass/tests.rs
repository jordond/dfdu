use super::*;

#[test]
fn prompts_are_told_apart() {
    let password = |a: &str| Kind::Password { account: a.into() };
    assert_eq!(kind("me@nas's password: "), password("me@nas"));
    assert_eq!(kind("(me@nas) Password: "), password("me@nas"));
    assert_eq!(
        kind("Enter passphrase for key '/Users/me/.ssh/id_ed25519': "),
        Kind::Passphrase {
            key: "/Users/me/.ssh/id_ed25519".into()
        }
    );
    let host = "The authenticity of host 'nas (10.0.0.2)' can't be established.\nED25519 key \
                fingerprint is SHA256:abc.\nAre you sure you want to continue connecting \
                (yes/no/[fingerprint])? ";
    assert_eq!(kind(host), Kind::HostKey);
    assert_eq!(
        kind("Are you sure you want to continue connecting (yes/no)? "),
        Kind::HostKey
    );
    // A server's text that reads like ssh's question is not taken for it.
    assert_eq!(
        kind("(me@nas) Are you sure you want to continue connecting (yes/no)? "),
        Kind::Other
    );
    assert_eq!(kind("Verification code: "), Kind::Other);
    assert_eq!(kind("(me@nas) Verification code: "), Kind::Other);
}

#[test]
fn tokens_are_random_hex() {
    let (a, b) = (token().expect("a token"), token().expect("a token"));
    assert_eq!(a.len(), 64);
    assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
    assert_ne!(a, b);
    assert!(same(a.as_bytes(), a.as_bytes()));
    assert!(!same(a.as_bytes(), b.as_bytes()));
    assert!(!same(b"ab", b"abc"));
}

/// Asks `server` from a thread with `token`, answering with `answer`; what the helper got.
fn round_trip(token: &str, answer: Option<&str>) -> (Option<String>, Option<String>) {
    let server = Askpass::start(&egui::Context::default()).expect("a server");
    let (addr, token) = (server.addr, token.replace("{right}", &server.token));
    let helper = std::thread::spawn(move || {
        ask_at(addr, &token, "me@nas's password: ", 42, false)
            .expect("asked")
            .map(|a| a.to_string())
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut asked = None;
    while !helper.is_finished() && std::time::Instant::now() < deadline {
        if let Some(ask) = server.next() {
            assert_eq!((ask.from, ask.confirm), (42, false));
            asked = Some(ask.prompt.clone());
            ask.answer(answer.map(|a| Zeroizing::new(a.into())));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    (asked, helper.join().expect("the helper"))
}

#[test]
fn the_window_answers_its_helper() {
    let (asked, got) = round_trip("{right}", Some("hunter2"));
    assert_eq!(asked.as_deref(), Some("me@nas's password:"));
    assert_eq!(got.as_deref(), Some("hunter2"));
}

#[test]
fn a_cancelled_question_has_no_answer() {
    let (asked, got) = round_trip("{right}", None);
    assert!(asked.is_some());
    assert_eq!(got, None);
}

#[test]
fn a_helper_without_the_token_is_not_asked() {
    let (asked, got) = round_trip("not-the-token", Some("hunter2"));
    assert_eq!(asked, None);
    assert_eq!(got, None);
}

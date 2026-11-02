use std::io::{ErrorKind, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::time::Duration;

use super::Authorization;
use super::service::{Failure, Status, registered};
use super::xpc::Anonymous;

const WAIT: Duration = Duration::from_secs(10);
const OTHER: &str = r#"identifier "app.dfdu" and anchor apple generic"#;
/// A right everyone holds without being asked: it falls under the `config.add.` rule, of
/// class allow. So the tests need no sheet, and change nothing in the policy database.
const ALLOWED: &str = "config.add.app.dfdu.test";

/// An authorization holding [`ALLOWED`].
fn auth() -> Authorization {
    let a = Authorization::create(std::ffi::CString::new(ALLOWED).unwrap()).unwrap();
    a.confirm().unwrap();
    a
}

/// A requirement this test binary meets: its code signing identifier (the linker signs it ad
/// hoc on Apple silicon). `None`, after saying so, when it is not signed.
fn this_binary() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let out = std::process::Command::new("codesign")
        .args(["-d", "--verbose=2"])
        .arg(&exe)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stderr);
    let id = text.lines().find_map(|l| l.strip_prefix("Identifier="));
    if id.is_none() {
        eprintln!("skipped: {} has no code signature: {text}", exe.display());
    }
    Some(format!("identifier \"{}\"", id?))
}

#[test]
fn the_link_reaches_the_helper() {
    let Some(me) = this_binary() else { return };
    let (tx, rx) = mpsc::channel();
    let server = Anonymous::listen(&me, ALLOWED, move |p| {
        tx.send(p).unwrap();
        Ok(())
    })
    .unwrap();
    let (mut mine, theirs) = UnixStream::pair().unwrap();
    server.connect(&me, theirs.as_fd(), &auth(), WAIT).unwrap();
    drop(theirs);
    let peer = rx.recv_timeout(WAIT).unwrap();
    assert_eq!(peer.euid, rustix::process::geteuid().as_raw());
    let flags = rustix::io::fcntl_getfd(&peer.link).unwrap();
    assert!(flags.contains(rustix::io::FdFlags::CLOEXEC));
    UnixStream::from(peer.link).write_all(b"hello").unwrap();
    let mut got = [0; 5];
    mine.read_exact(&mut got).unwrap();
    assert_eq!(&got, b"hello");
}

#[test]
fn a_failed_start_reaches_the_client() {
    let Some(me) = this_binary() else { return };
    let server = Anonymous::listen(&me, ALLOWED, |_| Err("no agent today".to_owned())).unwrap();
    let (_mine, theirs) = UnixStream::pair().unwrap();
    let err = server
        .connect(&me, theirs.as_fd(), &auth(), WAIT)
        .unwrap_err();
    assert!(err.to_string().contains("no agent today"), "{err}");
}

#[test]
fn a_panic_in_the_start_answers_an_error() {
    let Some(me) = this_binary() else { return };
    let server = Anonymous::listen(&me, ALLOWED, |_| panic!("on purpose")).unwrap();
    let (_mine, theirs) = UnixStream::pair().unwrap();
    let err = server
        .connect(&me, theirs.as_fd(), &auth(), WAIT)
        .unwrap_err();
    assert!(err.to_string().contains("failed starting"), "{err}");
    // The helper goes on serving.
    let (_mine, theirs) = UnixStream::pair().unwrap();
    assert!(server.connect(&me, theirs.as_fd(), &auth(), WAIT).is_err());
}

#[test]
fn a_link_that_is_not_a_socket_is_refused() {
    let Some(me) = this_binary() else { return };
    let (tx, rx) = mpsc::channel();
    let server = Anonymous::listen(&me, ALLOWED, move |p| {
        tx.send(p).unwrap();
        Ok(())
    })
    .unwrap();
    let (read, _write) = std::io::pipe().unwrap();
    let err = server
        .connect(&me, read.as_fd(), &auth(), WAIT)
        .unwrap_err();
    assert!(err.to_string().contains("not a socket"), "{err}");
    assert!(rx.try_recv().is_err());
}

#[test]
fn a_client_with_another_signature_is_refused() {
    let Some(me) = this_binary() else { return };
    let (tx, rx) = mpsc::channel();
    let server = Anonymous::listen(OTHER, ALLOWED, move |p| {
        tx.send(p).unwrap();
        Ok(())
    })
    .unwrap();
    let (_mine, theirs) = UnixStream::pair().unwrap();
    let err = server
        .connect(&me, theirs.as_fd(), &auth(), Duration::from_secs(2))
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ConnectionAborted, "{err}");
    assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
}

/// The boundary itself: a link sent straight away, by a client of its own making that does not
/// meet the requirement, never reaches `on_peer`.
#[test]
fn a_link_alone_from_another_signature_never_arrives() {
    if this_binary().is_none() {
        return;
    }
    let (tx, rx) = mpsc::channel();
    let server = Anonymous::listen(OTHER, ALLOWED, move |p| {
        tx.send(p).unwrap();
        Ok(())
    })
    .unwrap();
    let (_mine, theirs) = UnixStream::pair().unwrap();
    let err = server
        .send_link(theirs.as_fd(), None, Duration::from_secs(2))
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ConnectionAborted, "{err}");
    assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
}

/// A client that meets the requirement but sends no authorization, or bytes that are none, is
/// refused before `on_peer`.
#[test]
fn a_link_without_an_authorization_is_refused() {
    let Some(me) = this_binary() else { return };
    let (tx, rx) = mpsc::channel();
    let server = Anonymous::listen(&me, ALLOWED, move |p| {
        tx.send(p).unwrap();
        Ok(())
    })
    .unwrap();
    for (auth, why) in [
        (None, "no authorization"),
        (Some(&[7u8; 32][..]), "not valid"),
        (Some(&[7u8; 5][..]), "malformed"),
    ] {
        let (_mine, theirs) = UnixStream::pair().unwrap();
        let err = server.send_link(theirs.as_fd(), auth, WAIT).unwrap_err();
        assert!(err.to_string().contains(why), "{err}");
    }
    assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
}

/// An authorization without the right the helper asks for is refused.
#[test]
fn an_authorization_for_another_right_is_refused() {
    let Some(me) = this_binary() else { return };
    // Some machines grant every right without asking (CI runners whose policy database lets
    // the admin user through); there a refusal cannot be seen.
    let never = std::ffi::CString::new("app.dfdu.test.never-defined").unwrap();
    let empty = Authorization::create(never.clone()).unwrap();
    if super::authorization::check(Some(&empty.external_form().unwrap()), &never).is_ok() {
        eprintln!("skipped: this machine grants {never:?} without asking");
        return;
    }
    let (tx, rx) = mpsc::channel();
    let server = Anonymous::listen(&me, "app.dfdu.test.never-defined", move |p| {
        tx.send(p).unwrap();
        Ok(())
    })
    .unwrap();
    let (_mine, theirs) = UnixStream::pair().unwrap();
    let err = server
        .connect(&me, theirs.as_fd(), &auth(), WAIT)
        .unwrap_err();
    assert!(err.to_string().contains("not authorized"), "{err}");
    assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
}

#[test]
fn a_helper_with_another_signature_is_not_trusted() {
    let Some(me) = this_binary() else { return };
    let (tx, rx) = mpsc::channel();
    let server = Anonymous::listen(&me, ALLOWED, move |p| {
        tx.send(p).unwrap();
        Ok(())
    })
    .unwrap();
    let (_mine, theirs) = UnixStream::pair().unwrap();
    let err = server
        .connect(OTHER, theirs.as_fd(), &auth(), WAIT)
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::PermissionDenied, "{err}");
    assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
}

#[test]
fn an_invalid_requirement_is_an_error() {
    let err = Anonymous::listen("identifier (", ALLOWED, |_| Ok(()))
        .err()
        .unwrap();
    assert_eq!(err.kind(), ErrorKind::InvalidInput, "{err}");
}

#[test]
fn no_helper_is_not_found() {
    let (_mine, theirs) = UnixStream::pair().unwrap();
    let err = super::connect(
        "app.dfdu.helper.missing",
        "anchor apple generic",
        theirs.as_fd(),
        &auth(),
        WAIT,
    )
    .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound, "{err}");
}

#[test]
fn serving_a_service_launchd_did_not_give_is_an_error() {
    // As root, `serve` would define the right in the policy database.
    if rustix::process::geteuid().is_root() {
        eprintln!("skipped: run as root");
        return;
    }
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let right = super::Right {
            name: ALLOWED,
            prompt: "dfdu wants to test.",
            timeout: Duration::from_secs(300),
        };
        let _ = tx.send(super::serve(
            "app.dfdu.helper.missing",
            "anchor apple generic",
            &right,
            |_| Ok(()),
        ));
    });
    let err = rx.recv_timeout(WAIT).expect("serve returns");
    assert_eq!(err.kind(), ErrorKind::NotFound, "{err}");
}

fn failure(domain: &str, code: isize) -> Failure {
    Failure {
        domain: domain.into(),
        code,
        message: "Operation not permitted".into(),
    }
}

#[test]
fn registering_maps_waiting_for_approval_to_a_status() {
    let sm = "SMAppServiceErrorDomain";
    assert_eq!(
        registered(Err(failure(sm, 1)), Status::RequiresApproval).unwrap(),
        Status::RequiresApproval
    );
    assert_eq!(
        registered(Ok(()), Status::Enabled).unwrap(),
        Status::Enabled
    );
    assert_eq!(
        registered(Err(failure(sm, 12)), Status::Enabled).unwrap(),
        Status::Enabled
    );
    // Other errors stay errors, whatever the status after.
    let err = registered(Err(failure(sm, 3)), Status::RequiresApproval).unwrap_err();
    assert!(
        err.to_string().contains("SMAppServiceErrorDomain 3"),
        "{err}"
    );
    assert!(
        registered(
            Err(failure("NSPOSIXErrorDomain", 1)),
            Status::RequiresApproval
        )
        .is_err()
    );
    assert!(registered(Err(failure(sm, 1)), Status::Enabled).is_err());
    assert!(registered(Err(failure(sm, 1)), Status::NotRegistered).is_err());
}

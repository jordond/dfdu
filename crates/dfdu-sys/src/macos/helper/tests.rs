use std::io::{ErrorKind, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::time::Duration;

use super::service::{Failure, Status, registered};
use super::xpc::Anonymous;

const WAIT: Duration = Duration::from_secs(10);
const OTHER: &str = r#"identifier "app.dfdu" and anchor apple generic"#;

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
    let server = Anonymous::listen(&me, move |p| {
        tx.send(p).unwrap();
        Ok(())
    })
    .unwrap();
    let (mut mine, theirs) = UnixStream::pair().unwrap();
    server.connect(&me, theirs.as_fd(), WAIT).unwrap();
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
    let server = Anonymous::listen(&me, |_| Err("no agent today".to_owned())).unwrap();
    let (_mine, theirs) = UnixStream::pair().unwrap();
    let err = server.connect(&me, theirs.as_fd(), WAIT).unwrap_err();
    assert!(err.to_string().contains("no agent today"), "{err}");
}

#[test]
fn a_panic_in_the_start_answers_an_error() {
    let Some(me) = this_binary() else { return };
    let server = Anonymous::listen(&me, |_| panic!("on purpose")).unwrap();
    let (_mine, theirs) = UnixStream::pair().unwrap();
    let err = server.connect(&me, theirs.as_fd(), WAIT).unwrap_err();
    assert!(err.to_string().contains("failed starting"), "{err}");
    // The helper goes on serving.
    let (_mine, theirs) = UnixStream::pair().unwrap();
    assert!(server.connect(&me, theirs.as_fd(), WAIT).is_err());
}

#[test]
fn a_link_that_is_not_a_socket_is_refused() {
    let Some(me) = this_binary() else { return };
    let (tx, rx) = mpsc::channel();
    let server = Anonymous::listen(&me, move |p| {
        tx.send(p).unwrap();
        Ok(())
    })
    .unwrap();
    let (read, _write) = std::io::pipe().unwrap();
    let err = server.connect(&me, read.as_fd(), WAIT).unwrap_err();
    assert!(err.to_string().contains("not a socket"), "{err}");
    assert!(rx.try_recv().is_err());
}

#[test]
fn a_client_with_another_signature_is_refused() {
    let Some(me) = this_binary() else { return };
    let (tx, rx) = mpsc::channel();
    let server = Anonymous::listen(OTHER, move |p| {
        tx.send(p).unwrap();
        Ok(())
    })
    .unwrap();
    let (_mine, theirs) = UnixStream::pair().unwrap();
    let err = server
        .connect(&me, theirs.as_fd(), Duration::from_secs(2))
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
    let server = Anonymous::listen(OTHER, move |p| {
        tx.send(p).unwrap();
        Ok(())
    })
    .unwrap();
    let (_mine, theirs) = UnixStream::pair().unwrap();
    let err = server
        .send_link(theirs.as_fd(), Duration::from_secs(2))
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::ConnectionAborted, "{err}");
    assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
}

#[test]
fn a_helper_with_another_signature_is_not_trusted() {
    let Some(me) = this_binary() else { return };
    let (tx, rx) = mpsc::channel();
    let server = Anonymous::listen(&me, move |p| {
        tx.send(p).unwrap();
        Ok(())
    })
    .unwrap();
    let (_mine, theirs) = UnixStream::pair().unwrap();
    let err = server.connect(OTHER, theirs.as_fd(), WAIT).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::PermissionDenied, "{err}");
    assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
}

#[test]
fn an_invalid_requirement_is_an_error() {
    let err = Anonymous::listen("identifier (", |_| Ok(())).err().unwrap();
    assert_eq!(err.kind(), ErrorKind::InvalidInput, "{err}");
}

#[test]
fn no_helper_is_not_found() {
    let (_mine, theirs) = UnixStream::pair().unwrap();
    let err = super::connect(
        "app.dfdu.helper.missing",
        "anchor apple generic",
        theirs.as_fd(),
        WAIT,
    )
    .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound, "{err}");
}

#[test]
fn serving_a_service_launchd_did_not_give_is_an_error() {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(super::serve(
            "app.dfdu.helper.missing",
            "anchor apple generic",
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

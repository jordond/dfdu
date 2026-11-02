use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::time::Duration;

use super::service::{Failure, Status, registered};
use super::xpc::Anonymous;

const WAIT: Duration = Duration::from_secs(10);

/// A requirement this test binary meets: its code signing identifier (the linker signs it
/// ad hoc).
fn this_binary() -> String {
    let exe = std::env::current_exe().unwrap();
    let out = std::process::Command::new("codesign")
        .args(["-d", "--verbose=2"])
        .arg(&exe)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stderr);
    let id = text
        .lines()
        .find_map(|l| l.strip_prefix("Identifier="))
        .unwrap_or_else(|| panic!("no identifier for {}: {text}", exe.display()));
    format!("identifier \"{id}\"")
}

#[test]
fn the_link_reaches_the_helper() {
    let me = this_binary();
    let (tx, rx) = mpsc::channel();
    let server = Anonymous::listen(&me, move |p| tx.send(p).unwrap()).unwrap();
    let (mut mine, theirs) = UnixStream::pair().unwrap();
    server.connect(&me, theirs.as_fd(), WAIT).unwrap();
    drop(theirs);
    let peer = rx.recv_timeout(WAIT).unwrap();
    assert_eq!(peer.euid, rustix::process::geteuid().as_raw());
    UnixStream::from(peer.link).write_all(b"hello").unwrap();
    let mut got = [0; 5];
    mine.read_exact(&mut got).unwrap();
    assert_eq!(&got, b"hello");
}

#[test]
fn a_client_with_another_signature_is_refused() {
    let me = this_binary();
    let (tx, rx) = mpsc::channel();
    let server = Anonymous::listen(
        r#"identifier "app.dfdu" and anchor apple generic"#,
        move |p| tx.send(p).unwrap(),
    )
    .unwrap();
    let (_mine, theirs) = UnixStream::pair().unwrap();
    let err = server
        .connect(&me, theirs.as_fd(), Duration::from_secs(2))
        .unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::ConnectionAborted, "{err}");
    assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
}

#[test]
fn a_helper_with_another_signature_is_not_trusted() {
    let me = this_binary();
    let (tx, rx) = mpsc::channel();
    let server = Anonymous::listen(&me, move |p| tx.send(p).unwrap()).unwrap();
    let (_mine, theirs) = UnixStream::pair().unwrap();
    let err = server
        .connect(
            r#"identifier "app.dfdu" and anchor apple generic"#,
            theirs.as_fd(),
            WAIT,
        )
        .unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied, "{err}");
    assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
}

#[test]
fn an_invalid_requirement_is_an_error() {
    let err = Anonymous::listen("identifier (", |_| {}).err().unwrap();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "{err}");
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
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound, "{err}");
}

#[test]
fn serving_a_service_launchd_did_not_give_is_an_error() {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(super::serve(
            "app.dfdu.helper.missing",
            "anchor apple generic",
            |_| {},
        ));
    });
    let err = rx.recv_timeout(WAIT).expect("serve returns");
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound, "{err}");
}

fn failure(code: isize) -> Failure {
    Failure {
        domain: "SMAppServiceErrorDomain".into(),
        code,
        message: "Operation not permitted".into(),
    }
}

#[test]
fn registering_maps_waiting_for_approval_to_a_status() {
    assert_eq!(
        registered(Err(failure(1)), Status::RequiresApproval).unwrap(),
        Status::RequiresApproval
    );
    assert_eq!(
        registered(Ok(()), Status::Enabled).unwrap(),
        Status::Enabled
    );
    assert_eq!(
        registered(Err(failure(12)), Status::Enabled).unwrap(),
        Status::Enabled
    );
    let err = registered(Err(failure(3)), Status::NotRegistered).unwrap_err();
    assert!(
        err.to_string().contains("SMAppServiceErrorDomain 3"),
        "{err}"
    );
    assert!(registered(Err(failure(1)), Status::Enabled).is_err());
}

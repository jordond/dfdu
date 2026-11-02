//! The XPC handshake between the GUI and the helper daemon, over the C API in libSystem
//! (`<xpc/xpc.h>`). Two requests, each answered `{ok: true}` or `{error: "..."}`: first an empty
//! one, then `{link: <fd>, auth: <32 bytes>}`, one end of a socket pair the client made and the
//! external form of an administrator's authorization ([`super::authorization`]), answered once
//! the daemon has checked the authorization and started the agent on the socket. Both sides require a code signature of the other
//! (`xpc_connection_set_peer_code_signing_requirement`, macOS 12+). The daemon's requirement
//! drops every message of a client that does not meet it, which then sees the connection
//! interrupted. The client's requirement only screens what it receives, so a daemon that does
//! not meet it would still get a socket sent in the first message; the empty request comes
//! first so the socket is sent only after a reply passed the check.
//!
//! Ownership: every object an `xpc_*_create*` call returns is owned by the caller and released
//! once (`raw::Owned`, `raw::Active`); objects handed to an event handler are not, and are used
//! only during the call. Handler blocks are copied by XPC when set, and freed by it when the
//! connection is.

#![allow(unsafe_code)]

#[cfg(test)]
mod anonymous;
mod raw;

#[cfg(test)]
pub(super) use anonymous::Anonymous;

use std::ffi::{CStr, CString, c_void};
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use block2::RcBlock;

use super::authorization::{self, Authorization, Right};

use raw::{
    _xpc_error_connection_interrupted, _xpc_error_connection_invalid, _xpc_error_key_description,
    _xpc_error_termination_imminent, _xpc_type_connection, _xpc_type_dictionary, _xpc_type_error,
    Active, Handler, MACH_SERVICE_LISTENER, MACH_SERVICE_PRIVILEGED, Owned, Queue, XpcObject,
    is_peer_requirement_error, set_peer_requirement, xpc_connection_activate,
    xpc_connection_cancel, xpc_connection_create, xpc_connection_create_mach_service,
    xpc_connection_get_euid, xpc_connection_send_message, xpc_connection_send_message_with_reply,
    xpc_connection_set_event_handler, xpc_dictionary_create, xpc_dictionary_create_reply,
    xpc_dictionary_dup_fd, xpc_dictionary_get_bool, xpc_dictionary_get_data,
    xpc_dictionary_get_remote_connection, xpc_dictionary_get_string, xpc_dictionary_get_value,
    xpc_dictionary_set_bool, xpc_dictionary_set_data, xpc_dictionary_set_string,
    xpc_dictionary_set_value, xpc_fd_create, xpc_get_type,
};

const LINK: &CStr = c"link";
const AUTH: &CStr = c"auth";
const OK: &CStr = c"ok";
const ERROR: &CStr = c"error";

/// A client whose code signature met the daemon's requirement, and the socket it sent.
#[derive(Debug)]
pub struct Peer {
    /// The client's effective uid when it connected: the user the agent acts for. Never root's.
    pub euid: u32,
    /// The client's socket (checked to be one, close-on-exec): the agent's standard input and
    /// output.
    pub link: OwnedFd,
}

/// What `on_peer` makes of a [`Peer`]: `Err` with a message for the client when the agent
/// could not be started.
pub type Started = Result<(), String>;

/// Listens on the launchd mach service `service` (the daemon's `MachServices` key) and hands
/// every client that meets the code signing `requirement`, holds `right` (an administrator
/// authorized it) and sends a socket to `on_peer`, on a queue of XPC's: `on_peer` should start the agent and return. Its result is the client's
/// answer, so a failed start reaches the client as an error; a panic in it answers an error
/// too. Requests without a link are answered `ok` (the client checking who answers); a client
/// that sends more than one link gets an error for the later ones.
///
/// `right` is defined in the system's policy database first, when it is missing or differs,
/// which only root may do without asking.
///
/// Returns only when listening fails: an invalid requirement, `right` not defined, or launchd
/// not having the service for this process (the program was not started by launchd for it).
#[must_use]
pub fn serve(
    service: &str,
    requirement: &str,
    right: &Right<'_>,
    on_peer: impl Fn(Peer) -> Started + Send + Sync + 'static,
) -> io::Error {
    let (name, requirement, right_name) =
        match (cstring(service), checked(requirement), cstring(right.name)) {
            (Ok(n), Ok(r), Ok(a)) => (n, r, a),
            (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => return e,
        };
    if let Err(e) = authorization::define(right) {
        return e;
    }
    let queue = Queue::new(c"app.dfdu.helper");
    // SAFETY: `name` is NUL-terminated and `queue` a live queue (the connection retains it).
    // The new connection is ours and goes to `listen`, which owns it from then on.
    let listener = unsafe {
        xpc_connection_create_mach_service(name.as_ptr(), queue.0, MACH_SERVICE_LISTENER)
    };
    let (tx, rx) = mpsc::channel();
    let on_peer = Arc::new(on_peer);
    let _listener = match listen(listener, requirement, right_name, on_peer, move |e| {
        let _ = tx.send(e);
    }) {
        Ok(l) => l,
        Err(e) => return e,
    };
    rx.recv()
        .unwrap_or_else(|_| io::Error::other("the helper stopped listening"))
}

/// Sends `link` and `auth` to the daemon behind the mach service `service`, which must meet
/// the code signing `requirement`, and waits up to `timeout` for its answer. `auth` must hold
/// the daemon's right already ([`Authorization::confirm`] just before). The caller keeps `link`
/// (XPC sends a duplicate) and runs dfdu's protocol over its other end once this returns.
///
/// # Errors
///
/// `NotFound` when the daemon is not registered or not approved, `PermissionDenied` when its
/// signature does not meet `requirement` (no socket was sent then), `ConnectionAborted` when it
/// dropped the connection without answering, as it does to a client whose signature it does
/// not accept, `TimedOut` without an answer in time, and the daemon's own refusal, such as the
/// authorization not holding its right or the agent not starting.
pub fn connect(
    service: &str,
    requirement: &str,
    link: BorrowedFd<'_>,
    auth: &Authorization,
    timeout: Duration,
) -> io::Result<()> {
    let name = cstring(service)?;
    let requirement = cstring(requirement)?;
    let form = auth.external_form()?;
    let queue = Queue::new(c"app.dfdu.helper.client");
    // SAFETY: as in `serve`; the connection goes to `request`, which owns it from then on.
    let conn = unsafe {
        xpc_connection_create_mach_service(name.as_ptr(), queue.0, MACH_SERVICE_PRIVILEGED)
    };
    request(conn, &queue, Some(&requirement), link, Some(&form), timeout)
}

/// Starts the listener `listener` (new, inactive, ours): each new client gets `requirement`
/// and a handler for its messages, which checks that it holds `right`; listener errors other
/// than termination go to `on_error`.
fn listen(
    listener: XpcObject,
    requirement: CString,
    right: CString,
    on_peer: Arc<dyn Fn(Peer) -> Started + Send + Sync>,
    on_error: impl Fn(io::Error) + 'static,
) -> io::Result<Active> {
    let on_event: RcBlock<dyn Fn(XpcObject)> = RcBlock::new(move |event: XpcObject| {
        // SAFETY: `event` is the object XPC hands the handler, live for the call.
        let kind = unsafe { xpc_get_type(event) };
        if kind == &raw const _xpc_type_connection {
            accept(event, &requirement, &right, &on_peer);
        } else if event.cast_const() == &raw const _xpc_error_connection_invalid {
            on_error(io::Error::new(
                io::ErrorKind::NotFound,
                "launchd has not given this process the helper's service",
            ));
        } else if kind == &raw const _xpc_type_error
            && event.cast_const() != &raw const _xpc_error_termination_imminent
        {
            on_error(failure(event, "the helper's listener failed"));
        }
    });
    start(listener, None, &on_event)
}

/// Takes in a new client `peer` (not ours: XPC keeps it until it goes invalid).
fn accept(
    peer: XpcObject,
    requirement: &CStr,
    right: &CStr,
    on_peer: &Arc<dyn Fn(Peer) -> Started + Send + Sync>,
) {
    let served = AtomicBool::new(false);
    let right = right.to_owned();
    let on_peer = Arc::clone(on_peer);
    let on_message: RcBlock<dyn Fn(XpcObject)> = RcBlock::new(move |msg: XpcObject| {
        // SAFETY: `msg` is live for the call. Errors (the client going away) need nothing.
        if unsafe { xpc_get_type(msg) } != &raw const _xpc_type_dictionary {
            return;
        }
        // SAFETY: `msg` is a live dictionary and `LINK` NUL-terminated; the value is not ours
        // and only tested for NULL.
        if unsafe { xpc_dictionary_get_value(msg, LINK.as_ptr()) }.is_null() {
            answer(msg, &Ok(()));
            return;
        }
        let started = if served.swap(true, Ordering::Relaxed) {
            Err("one link per connection".to_owned())
        } else {
            take_link(msg, &right).and_then(|peer| {
                // A panic must not unwind into XPC; the daemon goes on serving.
                catch_unwind(AssertUnwindSafe(|| on_peer(peer)))
                    .unwrap_or_else(|_| Err("the helper failed starting the agent".to_owned()))
            })
        };
        answer(msg, &started);
    });
    let ignore: RcBlock<dyn Fn(XpcObject)> = RcBlock::new(|_: XpcObject| {});
    // SAFETY: `peer` is a new, inactive connection from the listener, live for this call,
    // and `requirement` is NUL-terminated (XPC copies it). The requirement is set before it
    // is activated. A peer refused it is activated with a handler that ignores everything and
    // canceled, as XPC requires of each one; XPC copies the handlers.
    unsafe {
        if set_peer_requirement(peer, requirement).is_err() {
            xpc_connection_set_event_handler(peer, &ignore);
            xpc_connection_activate(peer);
            xpc_connection_cancel(peer);
            return;
        }
        xpc_connection_set_event_handler(peer, &on_message);
        xpc_connection_activate(peer);
    }
}

/// The client and socket of the message `msg`, a dictionary live for the call, once its
/// authorization holds `right`.
fn take_link(msg: XpcObject, right: &CStr) -> Result<Peer, String> {
    // SAFETY: `msg` is a live dictionary; the remote connection it names is not ours and
    // lives at least as long as the message.
    let euid = unsafe {
        let remote = xpc_dictionary_get_remote_connection(msg);
        if remote.is_null() {
            return Err("no client".to_owned());
        }
        xpc_connection_get_euid(remote)
    };
    match euid {
        0 => return Err("the helper does not act for root".to_owned()),
        // What XPC reports when the client is gone.
        u32::MAX => return Err("the client has gone".to_owned()),
        _ => {}
    }
    authorization::check(data(msg, AUTH).as_deref(), right)?;
    // SAFETY: `LINK` is NUL-terminated; a descriptor returned is a new one, ours to close.
    let fd = unsafe { xpc_dictionary_dup_fd(msg, LINK.as_ptr()) };
    if fd < 0 {
        return Err("the message has no link".to_owned());
    }
    // SAFETY: `fd` is open and owned by nothing else.
    let dup = unsafe { OwnedFd::from_raw_fd(fd) };
    // A copy made close-on-exec, so no other child of the daemon inherits it (the agent gets
    // it as stdin and stdout, which `dup2` makes without the flag); `dup` closes on return.
    let link = rustix::io::fcntl_dupfd_cloexec(&dup, 0).map_err(|e| format!("the link: {e}"))?;
    let st = rustix::fs::fstat(&link).map_err(|e| format!("the link: {e}"))?;
    if rustix::fs::FileType::from_raw_mode(st.st_mode) != rustix::fs::FileType::Socket {
        return Err("the link is not a socket".to_owned());
    }
    Ok(Peer { euid, link })
}

/// Replies to `msg` with `{ok: true}`, or `{error}`.
fn answer(msg: XpcObject, started: &Started) {
    let error = started.as_ref().err().map(|e| {
        CString::new(e.replace('\0', " ")).unwrap_or_else(|_| c"the helper failed".to_owned())
    });
    // SAFETY: `msg` is a live dictionary. The reply, when there is one (`msg` asked for it),
    // is ours and released after sending, which retains what it needs; the remote connection
    // is not ours and lives as long as `msg`. Keys and values are NUL-terminated and copied.
    unsafe {
        let reply = xpc_dictionary_create_reply(msg);
        if reply.is_null() {
            return;
        }
        let reply = Owned(reply);
        match &error {
            None => xpc_dictionary_set_bool(reply.0, OK.as_ptr(), true),
            Some(e) => xpc_dictionary_set_string(reply.0, ERROR.as_ptr(), e.as_ptr()),
        }
        let remote = xpc_dictionary_get_remote_connection(msg);
        if !remote.is_null() {
            xpc_connection_send_message(remote, reply.0);
        }
    }
}

/// The bytes at `key` in the dictionary `msg` (live for the call), copied.
fn data(msg: XpcObject, key: &CStr) -> Option<Vec<u8>> {
    let mut len = 0;
    // SAFETY: `msg` is live and `key` NUL-terminated; the bytes returned, when not NULL, are
    // `len` bytes that belong to `msg` and are copied before this returns.
    unsafe {
        let p = xpc_dictionary_get_data(msg, key.as_ptr(), &mut len);
        (!p.is_null()).then(|| std::slice::from_raw_parts(p.cast::<u8>(), len).to_vec())
    }
}

/// Sends an empty request and then `link` and `auth` on `conn` (new, inactive, ours,
/// targeting `queue`, or NULL when it could not be made), each time waiting for the answer,
/// both within `timeout`.
fn request(
    conn: XpcObject,
    queue: &Queue,
    requirement: Option<&CStr>,
    link: BorrowedFd<'_>,
    auth: Option<&[u8]>,
    timeout: Duration,
) -> io::Result<()> {
    // Errors outside a reply need nothing: the reply handler gets them too.
    let ignore: RcBlock<dyn Fn(XpcObject)> = RcBlock::new(|_: XpcObject| {});
    let conn = start(conn, requirement, &ignore)?;
    let deadline = deadline(timeout);
    // SAFETY: an empty dictionary, ours.
    let hello = Owned(unsafe { xpc_dictionary_create(std::ptr::null(), std::ptr::null(), 0) });
    send(&conn, queue, &hello, deadline)?;
    send(&conn, queue, &link_message(link, auth)?, deadline)
}

/// `timeout` from now; one too long for an `Instant` is no timeout: a year.
fn deadline(timeout: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(timeout)
        .unwrap_or(now + Duration::from_secs(365 * 86_400))
}

/// The message `{link, auth}` (without `auth` when `None`), ours.
fn link_message(link: BorrowedFd<'_>, auth: Option<&[u8]>) -> io::Result<Owned> {
    // SAFETY: `link` is open for the call; XPC duplicates it into the new object, ours.
    let fd = unsafe { xpc_fd_create(link.as_raw_fd()) };
    if fd.is_null() {
        return Err(io::Error::other("could not wrap the link for XPC"));
    }
    let fd = Owned(fd);
    // SAFETY: an empty dictionary, ours; setting a value retains it.
    let msg = Owned(unsafe { xpc_dictionary_create(std::ptr::null(), std::ptr::null(), 0) });
    // SAFETY: `msg` and `fd` are live objects of ours and `LINK` NUL-terminated.
    unsafe { xpc_dictionary_set_value(msg.0, LINK.as_ptr(), fd.0) };
    if let Some(auth) = auth {
        // SAFETY: `msg` is ours, `AUTH` NUL-terminated, and `auth` valid for its length; XPC
        // copies the bytes.
        unsafe { xpc_dictionary_set_data(msg.0, AUTH.as_ptr(), auth.as_ptr().cast(), auth.len()) };
    }
    Ok(msg)
}

/// Sends the dictionary `msg` on `conn` and waits until `deadline` for the reply.
fn send(conn: &Active, queue: &Queue, msg: &Owned, deadline: Instant) -> io::Result<()> {
    let (tx, rx) = mpsc::sync_channel(1);
    let on_reply: RcBlock<dyn Fn(XpcObject)> = RcBlock::new(move |reply: XpcObject| {
        let _ = tx.try_send(reply_result(reply));
    });
    // SAFETY: `conn` is active and `msg` a dictionary (sending retains it); `queue` is live
    // (the connection retains it) and XPC copies the handler.
    unsafe { xpc_connection_send_message_with_reply(conn.0, msg.0, queue.0, &on_reply) };
    match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(r) => r,
        Err(mpsc::RecvTimeoutError::Timeout) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "the helper did not answer in time",
        )),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err(io::Error::other("the helper's answer was lost"))
        }
    }
}

/// What the daemon's reply `reply` (live for the call) means.
fn reply_result(reply: XpcObject) -> io::Result<()> {
    // SAFETY: `reply` is live; strings read from it are copied before the call returns.
    let kind = unsafe { xpc_get_type(reply) };
    if kind == &raw const _xpc_type_error {
        let is = |e: *const c_void| reply.cast_const() == e;
        return Err(if is(&raw const _xpc_error_connection_invalid) {
            io::Error::new(
                io::ErrorKind::NotFound,
                "the helper is not installed or not approved",
            )
        } else if is_peer_requirement_error(reply) {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the helper's signature does not match",
            )
        } else if is(&raw const _xpc_error_connection_interrupted) {
            io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "the helper closed the connection without answering \
                 (it may not accept this app's signature)",
            )
        } else {
            failure(reply, "the connection to the helper failed")
        });
    }
    if kind != &raw const _xpc_type_dictionary {
        return Err(io::Error::other("the helper's answer is malformed"));
    }
    // SAFETY: `reply` is a live dictionary and `OK` NUL-terminated.
    if unsafe { xpc_dictionary_get_bool(reply, OK.as_ptr()) } {
        return Ok(());
    }
    match string(reply, ERROR) {
        Some(e) => Err(io::Error::other(format!("the helper refused: {e}"))),
        None => Err(io::Error::other("the helper's answer is malformed")),
    }
}

/// An error for the XPC error dictionary `error` (live for the call), with its description.
fn failure(error: XpcObject, what: &str) -> io::Error {
    // SAFETY: `_xpc_error_key_description` is a constant NUL-terminated key set before main.
    let key = unsafe { CStr::from_ptr(_xpc_error_key_description) };
    match string(error, key) {
        Some(d) => io::Error::other(format!("{what}: {d}")),
        None => io::Error::other(what.to_owned()),
    }
}

/// The string at `key` in the dictionary `dict` (live for the call), copied.
fn string(dict: XpcObject, key: &CStr) -> Option<String> {
    // SAFETY: `dict` is live and `key` NUL-terminated; the string returned, when not NULL,
    // belongs to `dict` and is copied before this returns.
    unsafe {
        let s = xpc_dictionary_get_string(dict, key.as_ptr());
        (!s.is_null()).then(|| CStr::from_ptr(s).to_string_lossy().into_owned())
    }
}

/// Sets `handler` and the peer `requirement` on `conn` (new, inactive, ours, non-NULL) and
/// activates it. Activated even when the requirement is refused, since an inactive connection
/// must not be released; nothing has been sent on it then.
fn start(conn: XpcObject, requirement: Option<&CStr>, handler: &Handler) -> io::Result<Active> {
    if conn.is_null() {
        return Err(io::Error::other("could not make an XPC connection"));
    }
    // SAFETY: `conn` is inactive, so the requirement may still be set (once); XPC copies the
    // string and the handler. From activation on, `Active` releases it, once.
    unsafe {
        xpc_connection_set_event_handler(conn, handler);
        let set = requirement.map(|r| set_peer_requirement(conn, r));
        xpc_connection_activate(conn);
        let conn = Active(conn);
        set.transpose().map(|_| conn)
    }
}

/// `requirement`, when XPC accepts it, tried on a connection made only for that.
fn checked(requirement: &str) -> io::Result<CString> {
    let r = cstring(requirement)?;
    let ignore: RcBlock<dyn Fn(XpcObject)> = RcBlock::new(|_: XpcObject| {});
    // SAFETY: an anonymous listener on the default queue, ours, to `start`.
    let probe = unsafe { xpc_connection_create(std::ptr::null(), std::ptr::null_mut()) };
    start(probe, Some(&r), &ignore).map(|_| r)
}

fn cstring(s: &str) -> io::Result<CString> {
    CString::new(s).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a NUL in a name"))
}

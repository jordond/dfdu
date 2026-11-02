//! The XPC handshake between the GUI and the helper daemon, over the C API in libSystem
//! (`<xpc/xpc.h>`). One message each way: the client sends `{link: <fd>}`, one end of a socket
//! pair it made, and the daemon answers `{ok: true}` or `{error: "..."}`. Both sides require a
//! code signature of the other (`xpc_connection_set_peer_code_signing_requirement`, macOS 12+),
//! so a message from a client that does not meet the daemon's requirement never arrives, and
//! the client never hands its socket to a daemon that does not meet its own.
//!
//! Ownership: every object an `xpc_*_create*` call returns is owned by the caller and released
//! once ([`Owned`], [`Active`]); objects handed to an event handler are not, and are used only
//! during the call. Handler blocks are copied by XPC when set, and freed by it when the
//! connection is.

#![allow(unsafe_code)]

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use block2::{Block, RcBlock};

type XpcObject = *mut c_void;
type DispatchQueue = *mut c_void;
type Handler = Block<dyn Fn(XpcObject)>;

unsafe extern "C" {
    static _xpc_type_connection: c_void;
    static _xpc_type_dictionary: c_void;
    static _xpc_type_error: c_void;
    static _xpc_error_connection_invalid: c_void;
    static _xpc_error_connection_interrupted: c_void;
    static _xpc_error_peer_code_signing_requirement: c_void;
    static _xpc_error_termination_imminent: c_void;
    static _xpc_error_key_description: *const c_char;
    fn xpc_connection_create(name: *const c_char, queue: DispatchQueue) -> XpcObject;
    fn xpc_connection_create_mach_service(
        name: *const c_char,
        queue: DispatchQueue,
        flags: u64,
    ) -> XpcObject;
    fn xpc_connection_set_event_handler(connection: XpcObject, handler: &Handler);
    fn xpc_connection_set_peer_code_signing_requirement(
        connection: XpcObject,
        requirement: *const c_char,
    ) -> c_int;
    fn xpc_connection_activate(connection: XpcObject);
    fn xpc_connection_cancel(connection: XpcObject);
    fn xpc_connection_get_euid(connection: XpcObject) -> libc::uid_t;
    fn xpc_connection_send_message(connection: XpcObject, message: XpcObject);
    fn xpc_connection_send_message_with_reply(
        connection: XpcObject,
        message: XpcObject,
        queue: DispatchQueue,
        handler: &Handler,
    );
    fn xpc_dictionary_create(
        keys: *const *const c_char,
        values: *const XpcObject,
        count: usize,
    ) -> XpcObject;
    fn xpc_dictionary_create_reply(original: XpcObject) -> XpcObject;
    fn xpc_dictionary_get_remote_connection(dictionary: XpcObject) -> XpcObject;
    fn xpc_dictionary_set_value(dictionary: XpcObject, key: *const c_char, value: XpcObject);
    fn xpc_dictionary_set_bool(dictionary: XpcObject, key: *const c_char, value: bool);
    fn xpc_dictionary_set_string(dictionary: XpcObject, key: *const c_char, value: *const c_char);
    fn xpc_dictionary_get_bool(dictionary: XpcObject, key: *const c_char) -> bool;
    fn xpc_dictionary_get_string(dictionary: XpcObject, key: *const c_char) -> *const c_char;
    fn xpc_dictionary_dup_fd(dictionary: XpcObject, key: *const c_char) -> c_int;
    fn xpc_fd_create(fd: c_int) -> XpcObject;
    fn xpc_get_type(object: XpcObject) -> *const c_void;
    fn xpc_release(object: XpcObject);
    fn dispatch_queue_create(label: *const c_char, attr: *const c_void) -> DispatchQueue;
    fn dispatch_release(object: DispatchQueue);
}

#[cfg(test)]
unsafe extern "C" {
    fn xpc_endpoint_create(connection: XpcObject) -> XpcObject;
    fn xpc_connection_create_from_endpoint(endpoint: XpcObject) -> XpcObject;
    fn xpc_connection_set_target_queue(connection: XpcObject, queue: DispatchQueue);
}

const MACH_SERVICE_LISTENER: u64 = 1 << 0;
const MACH_SERVICE_PRIVILEGED: u64 = 1 << 1;
const LINK: &CStr = c"link";
const OK: &CStr = c"ok";
const ERROR: &CStr = c"error";

/// A client whose code signature met the daemon's requirement, and the socket it sent.
#[derive(Debug)]
pub struct Peer {
    /// The client's effective uid when it connected: the user the agent acts for.
    pub euid: u32,
    /// The client's socket: the agent's standard input and output.
    pub link: OwnedFd,
}

/// Listens on the launchd mach service `service` (the daemon's `MachServices` key) and hands
/// every client that meets the code signing `requirement` and sends a socket to `on_peer`, on
/// a queue of XPC's, one client at a time: `on_peer` should start the agent and return. A
/// client that sends more than one message gets an error for the later ones.
///
/// Returns only when listening fails: an invalid requirement, or launchd not having the
/// service for this process (the program was not started by launchd for it).
#[must_use]
pub fn serve(
    service: &str,
    requirement: &str,
    on_peer: impl Fn(Peer) + Send + Sync + 'static,
) -> io::Error {
    let (name, requirement) = match (cstring(service), checked(requirement)) {
        (Ok(n), Ok(r)) => (n, r),
        (Err(e), _) | (_, Err(e)) => return e,
    };
    let queue = Queue::new(c"app.dfdu.helper");
    // SAFETY: `name` is NUL-terminated and `queue` a live queue (the connection retains it).
    // The new connection is ours and goes to `listen`, which owns it from then on.
    let listener = unsafe {
        xpc_connection_create_mach_service(name.as_ptr(), queue.0, MACH_SERVICE_LISTENER)
    };
    let (tx, rx) = mpsc::channel();
    let _listener = match listen(listener, requirement, Arc::new(on_peer), move |e| {
        let _ = tx.send(e);
    }) {
        Ok(l) => l,
        Err(e) => return e,
    };
    rx.recv()
        .unwrap_or_else(|_| io::Error::other("the helper stopped listening"))
}

/// Sends `link` to the daemon behind the mach service `service`, which must meet the code
/// signing `requirement`, and waits up to `timeout` for its answer. The caller keeps `link`
/// (XPC sends a duplicate) and runs dfdu's protocol over its other end once this returns.
///
/// # Errors
///
/// `NotFound` when the daemon is not registered or not approved, `PermissionDenied` when its
/// signature does not meet `requirement`, `TimedOut` without an answer in time, and the
/// daemon's own refusal.
pub fn connect(
    service: &str,
    requirement: &str,
    link: BorrowedFd<'_>,
    timeout: Duration,
) -> io::Result<()> {
    let name = cstring(service)?;
    let requirement = cstring(requirement)?;
    let queue = Queue::new(c"app.dfdu.helper.client");
    // SAFETY: as in `serve`; the connection goes to `request`, which owns it from then on.
    let conn = unsafe {
        xpc_connection_create_mach_service(name.as_ptr(), queue.0, MACH_SERVICE_PRIVILEGED)
    };
    request(conn, &queue, &requirement, link, timeout)
}

/// An anonymous listener in this process, so the client and server code runs without launchd.
#[cfg(test)]
pub(super) struct Anonymous {
    listener: Active,
    _queue: Queue,
}

#[cfg(test)]
impl Anonymous {
    pub(super) fn listen(
        requirement: &str,
        on_peer: impl Fn(Peer) + Send + Sync + 'static,
    ) -> io::Result<Self> {
        let requirement = checked(requirement)?;
        let queue = Queue::new(c"app.dfdu.helper.test");
        // SAFETY: a NULL name makes an anonymous listener; `queue` is live. Ours, to `listen`.
        let listener = unsafe { xpc_connection_create(std::ptr::null(), queue.0) };
        let listener = listen(listener, requirement, Arc::new(on_peer), |_| {})?;
        Ok(Self {
            listener,
            _queue: queue,
        })
    }

    /// [`connect`], to this listener.
    pub(super) fn connect(
        &self,
        requirement: &str,
        link: BorrowedFd<'_>,
        timeout: Duration,
    ) -> io::Result<()> {
        let requirement = cstring(requirement)?;
        let queue = Queue::new(c"app.dfdu.helper.test.client");
        // SAFETY: the listener is an active anonymous listener; the endpoint is ours and
        // released when `endpoint` drops, after the connection made from it (which keeps
        // what it needs of it). The new connection is inactive, so its target queue may be
        // set; it goes to `request`.
        unsafe {
            let endpoint = Owned(xpc_endpoint_create(self.listener.0));
            let conn = xpc_connection_create_from_endpoint(endpoint.0);
            if !conn.is_null() {
                xpc_connection_set_target_queue(conn, queue.0);
            }
            request(conn, &queue, &requirement, link, timeout)
        }
    }
}

/// Starts the listener `listener` (new, inactive, ours): each new client gets `requirement`
/// and a handler for its first message; listener errors other than termination go to
/// `on_error`.
fn listen(
    listener: XpcObject,
    requirement: CString,
    on_peer: Arc<dyn Fn(Peer) + Send + Sync>,
    on_error: impl Fn(io::Error) + 'static,
) -> io::Result<Active> {
    let on_event: RcBlock<dyn Fn(XpcObject)> = RcBlock::new(move |event: XpcObject| {
        // SAFETY: `event` is the object XPC hands the handler, live for the call.
        let kind = unsafe { xpc_get_type(event) };
        if kind == &raw const _xpc_type_connection {
            accept(event, &requirement, &on_peer);
        } else if kind == &raw const _xpc_type_error
            && event.cast_const() != &raw const _xpc_error_termination_imminent
        {
            on_error(failure(event, "the helper's listener failed"));
        }
    });
    start(listener, None, &on_event)
}

/// Takes in a new client `peer` (not ours: XPC keeps it until it goes invalid).
fn accept(peer: XpcObject, requirement: &CStr, on_peer: &Arc<dyn Fn(Peer) + Send + Sync>) {
    let served = AtomicBool::new(false);
    let on_peer = Arc::clone(on_peer);
    let on_message: RcBlock<dyn Fn(XpcObject)> = RcBlock::new(move |msg: XpcObject| {
        // SAFETY: `msg` is live for the call. Errors (the client going away) need nothing.
        if unsafe { xpc_get_type(msg) } != &raw const _xpc_type_dictionary {
            return;
        }
        let peer = if served.swap(true, Ordering::Relaxed) {
            Err(c"one link per connection")
        } else {
            take_link(msg)
        };
        answer(msg, peer.as_ref().err().copied());
        if let Ok(peer) = peer {
            // A panic must not unwind into XPC; the daemon goes on serving.
            let _ = catch_unwind(AssertUnwindSafe(|| on_peer(peer)));
        }
    });
    // SAFETY: `peer` is a new, inactive connection from the listener, live for this call,
    // and `requirement` is NUL-terminated (XPC copies it). The requirement is set before it
    // is activated, and an inactive peer is canceled instead, as XPC requires of each one.
    unsafe {
        if xpc_connection_set_peer_code_signing_requirement(peer, requirement.as_ptr()) != 0 {
            xpc_connection_cancel(peer);
            return;
        }
        xpc_connection_set_event_handler(peer, &on_message);
        xpc_connection_activate(peer);
    }
}

/// The client and socket of the message `msg`, a dictionary live for the call.
fn take_link(msg: XpcObject) -> Result<Peer, &'static CStr> {
    // SAFETY: `msg` is a live dictionary; the remote connection it names is not ours and
    // lives at least as long as the message.
    let euid = unsafe {
        let remote = xpc_dictionary_get_remote_connection(msg);
        if remote.is_null() {
            return Err(c"no client");
        }
        xpc_connection_get_euid(remote)
    };
    // SAFETY: `LINK` is NUL-terminated; a descriptor returned is a new one, ours to close.
    let fd = unsafe { xpc_dictionary_dup_fd(msg, LINK.as_ptr()) };
    if fd < 0 {
        return Err(c"the message has no link");
    }
    // SAFETY: `fd` is open and owned by nothing else.
    let link = unsafe { OwnedFd::from_raw_fd(fd) };
    // So no other child of the daemon inherits it; the agent gets it as stdin and stdout,
    // which `dup2` makes without the flag.
    let _ = rustix::io::fcntl_setfd(&link, rustix::io::FdFlags::CLOEXEC);
    Ok(Peer { euid, link })
}

/// Replies to `msg` with `{ok: true}`, or `{error}` when there is one.
fn answer(msg: XpcObject, error: Option<&CStr>) {
    // SAFETY: `msg` is a live dictionary. The reply, when there is one (`msg` asked for it),
    // is ours and released after sending, which retains what it needs; the remote connection
    // is not ours and lives as long as `msg`. Keys and values are NUL-terminated and copied.
    unsafe {
        let reply = xpc_dictionary_create_reply(msg);
        if reply.is_null() {
            return;
        }
        let reply = Owned(reply);
        match error {
            None => xpc_dictionary_set_bool(reply.0, OK.as_ptr(), true),
            Some(e) => xpc_dictionary_set_string(reply.0, ERROR.as_ptr(), e.as_ptr()),
        }
        let remote = xpc_dictionary_get_remote_connection(msg);
        if !remote.is_null() {
            xpc_connection_send_message(remote, reply.0);
        }
    }
}

/// Sends `link` on `conn` (new, inactive, ours, targeting `queue`, or NULL when it could not
/// be made) and waits for the answer.
fn request(
    conn: XpcObject,
    queue: &Queue,
    requirement: &CStr,
    link: BorrowedFd<'_>,
    timeout: Duration,
) -> io::Result<()> {
    // Errors outside a reply need nothing: the reply handler gets them too.
    let ignore: RcBlock<dyn Fn(XpcObject)> = RcBlock::new(|_: XpcObject| {});
    let conn = start(conn, Some(requirement), &ignore)?;
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
    let (tx, rx) = mpsc::sync_channel(1);
    let on_reply: RcBlock<dyn Fn(XpcObject)> = RcBlock::new(move |reply: XpcObject| {
        let _ = tx.try_send(reply_result(reply));
    });
    // SAFETY: `conn` is active and `msg` a dictionary (sending retains it); `queue` is live
    // (the connection retains it) and XPC copies the handler.
    unsafe { xpc_connection_send_message_with_reply(conn.0, msg.0, queue.0, &on_reply) };
    match rx.recv_timeout(timeout) {
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
    unsafe {
        let kind = xpc_get_type(reply);
        if kind == &raw const _xpc_type_error {
            let is = |e: *const c_void| reply.cast_const() == e;
            return Err(if is(&raw const _xpc_error_connection_invalid) {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "the helper is not installed or not approved",
                )
            } else if is(&raw const _xpc_error_peer_code_signing_requirement) {
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "the helper's signature does not match",
                )
            } else if is(&raw const _xpc_error_connection_interrupted) {
                io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "the helper stopped before it answered",
                )
            } else {
                failure(reply, "the connection to the helper failed")
            });
        }
        if kind != &raw const _xpc_type_dictionary {
            return Err(io::Error::other("the helper's answer is malformed"));
        }
        if xpc_dictionary_get_bool(reply, OK.as_ptr()) {
            return Ok(());
        }
        match string(reply, ERROR) {
            Some(e) => Err(io::Error::other(format!("the helper refused: {e}"))),
            None => Err(io::Error::other("the helper's answer is malformed")),
        }
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
        let refused = requirement
            .is_some_and(|r| xpc_connection_set_peer_code_signing_requirement(conn, r.as_ptr()) != 0);
        xpc_connection_activate(conn);
        let conn = Active(conn);
        if refused {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the code signing requirement is invalid",
            ));
        }
        Ok(conn)
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

/// An XPC object of ours, released on drop.
struct Owned(XpcObject);

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the one reference we hold, released once.
            unsafe { xpc_release(self.0) };
        }
    }
}

/// An activated connection of ours: canceled (its handler gets a last invalid event) and
/// released on drop.
struct Active(XpcObject);

impl Drop for Active {
    fn drop(&mut self) {
        // SAFETY: `self.0` is an active connection whose one reference we hold.
        unsafe {
            xpc_connection_cancel(self.0);
            xpc_release(self.0);
        }
    }
}

/// A serial dispatch queue of ours, released on drop (connections retain their own).
struct Queue(DispatchQueue);

impl Queue {
    fn new(label: &CStr) -> Self {
        // SAFETY: `label` is NUL-terminated (copied); NULL attributes make a serial queue.
        Self(unsafe { dispatch_queue_create(label.as_ptr(), std::ptr::null()) })
    }
}

impl Drop for Queue {
    fn drop(&mut self) {
        // SAFETY: the one reference we hold, from `dispatch_queue_create`.
        unsafe { dispatch_release(self.0) };
    }
}

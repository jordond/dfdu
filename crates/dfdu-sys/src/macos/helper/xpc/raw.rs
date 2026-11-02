//! The XPC and dispatch declarations, the owners that release what XPC returns, and the
//! symbols new in macOS 12, looked up at run time: a direct reference to one would keep every
//! program linking this crate from starting on macOS 11.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::io;
use std::sync::OnceLock;

use block2::Block;

pub(super) type XpcObject = *mut c_void;
pub(super) type DispatchQueue = *mut c_void;
pub(super) type Handler = Block<dyn Fn(XpcObject)>;

unsafe extern "C" {
    pub(super) static _xpc_type_connection: c_void;
    pub(super) static _xpc_type_dictionary: c_void;
    pub(super) static _xpc_type_error: c_void;
    pub(super) static _xpc_error_connection_invalid: c_void;
    pub(super) static _xpc_error_connection_interrupted: c_void;
    pub(super) static _xpc_error_termination_imminent: c_void;
    pub(super) static _xpc_error_key_description: *const c_char;
    pub(super) fn xpc_connection_create(name: *const c_char, queue: DispatchQueue) -> XpcObject;
    pub(super) fn xpc_connection_create_mach_service(
        name: *const c_char,
        queue: DispatchQueue,
        flags: u64,
    ) -> XpcObject;
    pub(super) fn xpc_connection_set_event_handler(connection: XpcObject, handler: &Handler);
    pub(super) fn xpc_connection_activate(connection: XpcObject);
    pub(super) fn xpc_connection_cancel(connection: XpcObject);
    pub(super) fn xpc_connection_get_euid(connection: XpcObject) -> libc::uid_t;
    pub(super) fn xpc_connection_send_message(connection: XpcObject, message: XpcObject);
    pub(super) fn xpc_connection_send_message_with_reply(
        connection: XpcObject,
        message: XpcObject,
        queue: DispatchQueue,
        handler: &Handler,
    );
    pub(super) fn xpc_dictionary_create(
        keys: *const *const c_char,
        values: *const XpcObject,
        count: usize,
    ) -> XpcObject;
    pub(super) fn xpc_dictionary_create_reply(original: XpcObject) -> XpcObject;
    pub(super) fn xpc_dictionary_get_remote_connection(dictionary: XpcObject) -> XpcObject;
    pub(super) fn xpc_dictionary_set_value(
        dictionary: XpcObject,
        key: *const c_char,
        value: XpcObject,
    );
    pub(super) fn xpc_dictionary_set_bool(dictionary: XpcObject, key: *const c_char, value: bool);
    pub(super) fn xpc_dictionary_set_string(
        dictionary: XpcObject,
        key: *const c_char,
        value: *const c_char,
    );
    pub(super) fn xpc_dictionary_get_bool(dictionary: XpcObject, key: *const c_char) -> bool;
    pub(super) fn xpc_dictionary_get_data(
        dictionary: XpcObject,
        key: *const c_char,
        length: *mut usize,
    ) -> *const c_void;
    pub(super) fn xpc_dictionary_set_data(
        dictionary: XpcObject,
        key: *const c_char,
        bytes: *const c_void,
        length: usize,
    );
    pub(super) fn xpc_dictionary_get_value(dictionary: XpcObject, key: *const c_char) -> XpcObject;
    pub(super) fn xpc_dictionary_get_string(
        dictionary: XpcObject,
        key: *const c_char,
    ) -> *const c_char;
    pub(super) fn xpc_dictionary_dup_fd(dictionary: XpcObject, key: *const c_char) -> c_int;
    pub(super) fn xpc_fd_create(fd: c_int) -> XpcObject;
    pub(super) fn xpc_get_type(object: XpcObject) -> *const c_void;
    fn xpc_release(object: XpcObject);
    fn dispatch_queue_create(label: *const c_char, attr: *const c_void) -> DispatchQueue;
    fn dispatch_release(object: DispatchQueue);
}

#[cfg(test)]
unsafe extern "C" {
    pub(super) fn xpc_endpoint_create(connection: XpcObject) -> XpcObject;
    pub(super) fn xpc_connection_create_from_endpoint(endpoint: XpcObject) -> XpcObject;
    pub(super) fn xpc_connection_set_target_queue(connection: XpcObject, queue: DispatchQueue);
}

pub(super) const MACH_SERVICE_LISTENER: u64 = 1 << 0;
pub(super) const MACH_SERVICE_PRIVILEGED: u64 = 1 << 1;

/// An XPC object of ours, released on drop.
pub(super) struct Owned(pub(super) XpcObject);

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
pub(super) struct Active(pub(super) XpcObject);

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
pub(super) struct Queue(pub(super) DispatchQueue);

impl Queue {
    pub(super) fn new(label: &CStr) -> Self {
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

/// `RTLD_DEFAULT` of `<dlfcn.h>`: look in every image loaded.
const RTLD_DEFAULT: *mut c_void = std::ptr::without_provenance_mut(usize::MAX - 1);

/// The address of the symbol `name` in a loaded image, NULL when there is none.
fn symbol(name: &CStr) -> *mut c_void {
    // SAFETY: `name` is NUL-terminated; the result is only an address.
    unsafe { libc::dlsym(RTLD_DEFAULT, name.as_ptr()) }
}

/// Whether `object` is `XPC_ERROR_PEER_CODE_SIGNING_REQUIREMENT` (macOS 12+): a reply in place
/// of the peer's, which did not meet the requirement. Never, where the symbol is missing.
pub(super) fn is_peer_requirement_error(object: XpcObject) -> bool {
    // The address, as an integer so the static is `Sync`; 0 when missing.
    static ERROR: OnceLock<usize> = OnceLock::new();
    let error = *ERROR.get_or_init(|| symbol(c"_xpc_error_peer_code_signing_requirement").addr());
    error != 0 && object.addr() == error
}

type SetRequirement = unsafe extern "C" fn(XpcObject, *const c_char) -> c_int;

/// `xpc_connection_set_peer_code_signing_requirement(conn, requirement)` (macOS 12+).
///
/// # Safety
///
/// `conn` must be a live connection not yet activated, with no requirement set.
pub(super) unsafe fn set_peer_requirement(conn: XpcObject, requirement: &CStr) -> io::Result<()> {
    static SET: OnceLock<Option<SetRequirement>> = OnceLock::new();
    let set = SET.get_or_init(|| {
        let f = symbol(c"xpc_connection_set_peer_code_signing_requirement");
        // SAFETY: a non-NULL result is that function, of the signature `<xpc/connection.h>`
        // gives it, which `SetRequirement` is.
        (!f.is_null()).then(|| unsafe { std::mem::transmute::<*mut c_void, SetRequirement>(f) })
    });
    let Some(set) = set else {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "checking the helper's code signature needs macOS 12 or later",
        ));
    };
    // SAFETY: as the caller promises; `requirement` is NUL-terminated and copied by XPC.
    if unsafe { set(conn, requirement.as_ptr()) } != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the code signing requirement is invalid",
        ));
    }
    Ok(())
}

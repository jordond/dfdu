//! Names are stored as raw bytes in `os_str_bytes`' encoding: the OS bytes on Unix, a lossless
//! encoding of the UTF-16 name on Windows. They become `OsStr` and `PathBuf` again only where a
//! path is handed to the OS, through these two functions.

use std::borrow::Cow;
use std::ffi::OsStr;

use os_str_bytes::OsStrBytes;

/// Bytes to store for a name or a root path.
pub fn name_bytes(s: &OsStr) -> Cow<'_, [u8]> {
    s.to_raw_bytes()
}

/// The name a stored byte string came from. Only bytes produced by [`name_bytes`] are valid; the
/// scanner never truncates names, so anything else is a bug and panics.
pub fn name_os(b: &[u8]) -> Cow<'_, OsStr> {
    OsStr::assert_from_raw_bytes(b)
}

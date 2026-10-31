//! The free space the Finder shows for a volume, from CoreFoundation's
//! `kCFURLVolumeAvailableCapacityForImportantUsageKey`: `f_bavail` and what the system can
//! purge when it needs the room.

#![allow(unsafe_code)]

use std::ffi::c_void;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

type CFTypeRef = *const c_void;
type CFAllocatorRef = *const c_void;
type CFStringRef = *const c_void;
type CFURLRef = *const c_void;
type CFErrorRef = *mut c_void;

/// `kCFNumberSInt64Type`.
const NUMBER_SINT64: isize = 4;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFURLVolumeAvailableCapacityForImportantUsageKey: CFStringRef;
    fn CFURLCreateFromFileSystemRepresentation(
        alloc: CFAllocatorRef,
        buffer: *const u8,
        len: isize,
        is_directory: u8,
    ) -> CFURLRef;
    fn CFURLCopyResourcePropertyForKey(
        url: CFURLRef,
        key: CFStringRef,
        value: *mut CFTypeRef,
        error: *mut CFErrorRef,
    ) -> u8;
    fn CFGetTypeID(cf: CFTypeRef) -> usize;
    fn CFNumberGetTypeID() -> usize;
    fn CFNumberGetValue(number: CFTypeRef, kind: isize, value: *mut c_void) -> u8;
    fn CFRelease(cf: CFTypeRef);
}

/// The space available for important use on the volume at `path` (a directory), in bytes:
/// what the Finder shows as available. `None` when the system does not say.
pub(crate) fn important_available(path: &Path) -> Option<u64> {
    let bytes = path.as_os_str().as_bytes();
    let len = isize::try_from(bytes.len()).ok()?;
    // SAFETY: `bytes` is valid for reads of `len` bytes during the call, which copies them; a
    // null allocator is the default one. The URL is ours (Create rule) and released below.
    let url = unsafe {
        CFURLCreateFromFileSystemRepresentation(std::ptr::null(), bytes.as_ptr(), len, 1)
    };
    if url.is_null() {
        return None;
    }
    let mut value: CFTypeRef = std::ptr::null();
    // SAFETY: `url` is a live CFURL; the key is a constant CoreFoundation keeps for the life of
    // the process; `value` is valid for one write of a CFTypeRef; a null error pointer asks for
    // no error object, so there is none to release.
    let copied = unsafe {
        CFURLCopyResourcePropertyForKey(
            url,
            kCFURLVolumeAvailableCapacityForImportantUsageKey,
            &raw mut value,
            std::ptr::null_mut(),
        )
    };
    // SAFETY: `url` came from the Create call above and is not used after this.
    unsafe { CFRelease(url) };
    if copied == 0 || value.is_null() {
        return None;
    }
    let mut n: i64 = 0;
    // SAFETY: `value` is a live CF object that is ours (Copy rule). It is read as a CFNumber
    // only once its type id says it is one, into an `i64` as `NUMBER_SINT64` asks, and released
    // after, not used again.
    let read = unsafe {
        let read = CFGetTypeID(value) == CFNumberGetTypeID()
            && CFNumberGetValue(value, NUMBER_SINT64, (&raw mut n).cast()) != 0;
        CFRelease(value);
        read
    };
    if read { u64::try_from(n).ok() } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The root volume's space for important use is known, and no more than the volume holds.
    #[test]
    fn the_root_volume_says_what_is_available() {
        let n = important_available(Path::new("/")).expect("the root volume's capacity");
        let space = crate::macos::statfs_space(Path::new("/")).expect("statfs /");
        assert!(n > 0 && n <= space.total, "{n} of {}", space.total);
    }

    /// A path that does not exist has no volume to ask about.
    #[test]
    fn a_missing_path_has_no_capacity() {
        assert_eq!(
            important_available(Path::new("/nonexistent/dfdu-capacity-test")),
            None
        );
    }
}

//! What the Finder shows of a volume, from CoreFoundation's URL resource properties: its name
//! (`kCFURLVolumeLocalizedNameKey`), and its free space
//! (`kCFURLVolumeAvailableCapacityForImportantUsageKey`: `f_bavail` and what the system can
//! purge when it needs the room).

#![allow(unsafe_code)]

use std::ffi::{CStr, c_char, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

type CFTypeRef = *const c_void;
type CFAllocatorRef = *const c_void;
type CFStringRef = *const c_void;
type CFURLRef = *const c_void;
type CFErrorRef = *mut c_void;

/// `kCFNumberSInt64Type`.
const NUMBER_SINT64: isize = 4;
/// `kCFStringEncodingUTF8`.
const ENCODING_UTF8: u32 = 0x0800_0100;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFURLVolumeAvailableCapacityForImportantUsageKey: CFStringRef;
    static kCFURLVolumeLocalizedNameKey: CFStringRef;
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
    fn CFStringGetTypeID() -> usize;
    fn CFStringGetLength(s: CFStringRef) -> isize;
    fn CFStringGetMaximumSizeForEncoding(len: isize, encoding: u32) -> isize;
    fn CFStringGetCString(s: CFStringRef, buffer: *mut c_char, size: isize, encoding: u32) -> u8;
    fn CFRelease(cf: CFTypeRef);
}

/// A CF object that is ours to release (Create or Copy rule), released when dropped.
struct Owned(CFTypeRef);

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: `Owned` is only made from a non-null object we own, and nothing uses the
        // pointer after this.
        unsafe { CFRelease(self.0) };
    }
}

/// The resource property `key` of the volume at `path` (a directory). `None` when the system
/// does not say.
///
/// # Safety
///
/// `key` must be one of CoreFoundation's constant URL resource keys.
unsafe fn volume_property(path: &Path, key: CFStringRef) -> Option<Owned> {
    let bytes = path.as_os_str().as_bytes();
    let len = isize::try_from(bytes.len()).ok()?;
    // SAFETY: `bytes` is valid for reads of `len` bytes during the call, which copies them; a
    // null allocator is the default one. The URL is ours (Create rule) and released on drop.
    let url = unsafe {
        CFURLCreateFromFileSystemRepresentation(std::ptr::null(), bytes.as_ptr(), len, 1)
    };
    if url.is_null() {
        return None;
    }
    let url = Owned(url);
    let mut value: CFTypeRef = std::ptr::null();
    // SAFETY: `url` is a live CFURL; the caller passes a constant key CoreFoundation keeps for
    // the life of the process; `value` is valid for one write of a CFTypeRef; a null error
    // pointer asks for no error object, so there is none to release.
    let copied = unsafe {
        CFURLCopyResourcePropertyForKey(url.0, key, &raw mut value, std::ptr::null_mut())
    };
    // A value that was copied is ours (Copy rule).
    (copied != 0 && !value.is_null()).then(|| Owned(value))
}

/// The space available for important use on the volume at `path` (a directory), in bytes:
/// what the Finder shows as available. `None` when the system does not say.
pub(crate) fn important_available(path: &Path) -> Option<u64> {
    // SAFETY: the key is CoreFoundation's constant, there for the life of the process.
    let value =
        unsafe { volume_property(path, kCFURLVolumeAvailableCapacityForImportantUsageKey) }?;
    let mut n: i64 = 0;
    // SAFETY: `value` is a live CF object. It is read as a CFNumber only once its type id says
    // it is one, into an `i64` as `NUMBER_SINT64` asks.
    let read = unsafe {
        CFGetTypeID(value.0) == CFNumberGetTypeID()
            && CFNumberGetValue(value.0, NUMBER_SINT64, (&raw mut n).cast()) != 0
    };
    if read { u64::try_from(n).ok() } else { None }
}

/// The name of the volume at `path` (a directory) as the Finder shows it (`Macintosh HD`).
/// `None` when the system does not say.
pub(crate) fn volume_name(path: &Path) -> Option<String> {
    // SAFETY: the key is CoreFoundation's constant, there for the life of the process.
    let value = unsafe { volume_property(path, kCFURLVolumeLocalizedNameKey) }?;
    // SAFETY: `value` is a live CF object, checked to be a CFString before it is read as one.
    let size = unsafe {
        if CFGetTypeID(value.0) != CFStringGetTypeID() {
            return None;
        }
        CFStringGetMaximumSizeForEncoding(CFStringGetLength(value.0), ENCODING_UTF8)
    };
    // Room for the terminating NUL.
    let size = usize::try_from(size).ok()?.checked_add(1)?;
    let mut buf = vec![0u8; size];
    // SAFETY: `value` is a live CFString; `buf` is valid for writes of `size` bytes, which the
    // call is told, and it writes a NUL-terminated string within them.
    let ok = unsafe {
        CFStringGetCString(
            value.0,
            buf.as_mut_ptr().cast(),
            isize::try_from(size).ok()?,
            ENCODING_UTF8,
        )
    };
    if ok == 0 {
        return None;
    }
    let name = CStr::from_bytes_until_nul(&buf).ok()?.to_str().ok()?;
    (!name.is_empty()).then(|| name.to_owned())
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

    /// The root volume has a name (`Macintosh HD`, unless the user renamed it).
    #[test]
    fn the_root_volume_has_a_name() {
        let name = volume_name(Path::new("/")).expect("the root volume's name");
        assert!(!name.is_empty());
    }

    /// A path that does not exist has no volume to name.
    #[test]
    fn a_missing_path_has_no_name() {
        assert_eq!(volume_name(Path::new("/nonexistent/dfdu-name-test")), None);
    }
}

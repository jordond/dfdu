//! Names are stored as raw bytes in `os_str_bytes`' encoding: the OS bytes on Unix, a lossless
//! encoding of the UTF-16 name on Windows. They become `OsStr` and `PathBuf` again only where a
//! path is handed to the OS, through these two functions.

use std::borrow::Cow;
use std::ffi::OsStr;

use os_str_bytes::OsStrBytes;

/// Bytes to store for a name or a root path.
#[must_use]
pub fn name_bytes(s: &OsStr) -> Cow<'_, [u8]> {
    s.to_raw_bytes()
}

/// The name a stored byte string came from. Only bytes produced by [`name_bytes`] are valid; the
/// scanner never truncates names, so anything else is a bug and panics.
#[must_use]
pub fn name_os(b: &[u8]) -> Cow<'_, OsStr> {
    OsStr::assert_from_raw_bytes(b)
}

/// Appends the bytes [`name_bytes`] gives on Windows for the UTF-16 name `w`: WTF-8, which is
/// UTF-8 with each unpaired surrogate written as its own three-byte sequence. Backends that
/// read UTF-16 names from the OS use this instead of building an `OsString` per name.
pub fn name_bytes_wide(w: &[u16], out: &mut Vec<u8>) {
    for c in char::decode_utf16(w.iter().copied()) {
        match c {
            Ok(c) => out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes()),
            Err(e) => {
                let s = e.unpaired_surrogate();
                out.extend_from_slice(&[
                    0xE0 | (s >> 12) as u8,
                    0x80 | ((s >> 6) & 0x3F) as u8,
                    0x80 | (s & 0x3F) as u8,
                ]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_names() {
        let mut out = Vec::new();
        // "a", "é", U+1F600 as a surrogate pair, a lone high and a lone low surrogate.
        name_bytes_wide(
            &[0x61, 0xE9, 0xD83D, 0xDE00, 0xD800, 0x62, 0xDFFF],
            &mut out,
        );
        assert_eq!(
            out,
            b"a\xC3\xA9\xF0\x9F\x98\x80\xED\xA0\x80b\xED\xBF\xBF".to_vec()
        );
    }

    /// The same bytes as `os_str_bytes`, which the portable backend uses.
    #[cfg(windows)]
    #[test]
    fn wide_names_match_name_bytes() {
        use std::os::windows::ffi::OsStringExt;
        for w in [
            &[0x61u16, 0x62][..],
            &[0xD800],
            &[0xDC00, 0xD800],
            &[0xD83D, 0xDE00, 0xDFFF, 0x7A],
            &[0xFFFF, 0x1, 0x7FF, 0x800],
        ] {
            let mut out = Vec::new();
            name_bytes_wide(w, &mut out);
            let os = std::ffi::OsString::from_wide(w);
            assert_eq!(out, &*name_bytes(&os), "{w:x?}");
            assert_eq!(&*name_os(&out), os.as_os_str());
        }
    }
}

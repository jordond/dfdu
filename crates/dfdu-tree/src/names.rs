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

/// The name a stored byte string came from. On Unix every byte string is a name. On Windows only
/// what [`name_bytes`] gives converts exactly; anything else, such as a non-UTF-8 name a Unix
/// remote host sent or a damaged wire payload, converts with each invalid sequence replaced by
/// U+FFFD, as names are displayed, rather than panicking.
#[must_use]
pub fn name_os(b: &[u8]) -> Cow<'_, OsStr> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        match std::str::from_utf8(b) {
            Ok(s) => Cow::Borrowed(OsStr::new(s)),
            Err(_) => Cow::Owned(std::ffi::OsString::from_wide(&wide_of(b))),
        }
    }
    #[cfg(not(windows))]
    OsStr::assert_from_raw_bytes(b)
}

/// The UTF-16 name WTF-8 bytes stand for ([`name_bytes_wide`] reversed), with U+FFFD for each
/// sequence that is neither UTF-8 nor a surrogate, where `String::from_utf8_lossy` puts them.
#[cfg(any(windows, test))]
fn wide_of(b: &[u8]) -> Vec<u16> {
    let mut out = Vec::with_capacity(b.len());
    let mut rest = b;
    loop {
        let (ok, bad) = match std::str::from_utf8(rest) {
            Ok(s) => (s, None),
            Err(e) => {
                let (ok, bad) = rest.split_at(e.valid_up_to());
                let ok = std::str::from_utf8(ok).unwrap_or_default();
                (ok, Some((bad, e.error_len().unwrap_or(bad.len()))))
            }
        };
        out.extend(ok.encode_utf16());
        let Some((bad, len)) = bad else {
            return out;
        };
        // An unpaired surrogate as WTF-8 writes it: ED A0..BF 80..BF.
        if let [0xED, b1 @ 0xA0..=0xBF, b2 @ 0x80..=0xBF, ..] = *bad {
            out.push(0xD000 | (u16::from(b1 & 0x3F) << 6) | u16::from(b2 & 0x3F));
            rest = &bad[3..];
        } else {
            out.push(0xFFFD);
            rest = &bad[len..];
        }
    }
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

    /// Bytes no Windows name has (a Unix name that is not UTF-8) convert as displayed. On Unix
    /// they are the name itself.
    #[test]
    fn foreign_names_convert_without_panicking() {
        // A Latin-1 name, a surrogate cut after two of its three bytes, a cut character.
        for (b, shown) in [
            (&b"caf\xe9.txt"[..], "caf\u{fffd}.txt"),
            (b"a\xED\xA0", "a\u{fffd}\u{fffd}"),
            (b"\xE2\x82", "\u{fffd}"),
        ] {
            assert_eq!(
                wide_of(b),
                shown.encode_utf16().collect::<Vec<_>>(),
                "{b:x?}"
            );
            assert_eq!(String::from_utf8_lossy(b), shown);
            let os = name_os(b);
            if cfg!(windows) {
                assert_eq!(os, OsStr::new(shown));
            } else {
                assert_eq!(&*name_bytes(&os), b);
            }
        }
        // Too short to hold a surrogate, every two bytes decode as `from_utf8_lossy` has them.
        for b in (0..=u16::MAX).map(u16::to_le_bytes) {
            let lossy: Vec<u16> = String::from_utf8_lossy(&b).encode_utf16().collect();
            assert_eq!(wide_of(&b), lossy, "{b:x?}");
        }
    }

    /// WTF-8 decodes to the UTF-16 it was written from, surrogates and all.
    #[test]
    fn wide_names_decode() {
        let w = [
            0x61, 0xE9, 0xD83D, 0xDE00, 0xD800, 0x62, 0xDFFF, 0x7FF, 0x800, 0xFFFF,
        ];
        let mut out = Vec::new();
        name_bytes_wide(&w, &mut out);
        assert_eq!(wide_of(&out), w);
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

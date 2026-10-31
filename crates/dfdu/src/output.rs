//! What the commands print, shared between them: the JSON documents ([`json`]), the tables of
//! `scan` and `import` ([`table`]), and what the scan did not enter ([`outside`]). Here, how
//! bytes and paths are written in JSON: as UTF-8 text, with the raw bytes in base64 beside
//! them when they are not valid UTF-8.

pub mod json;
pub mod outside;
pub mod table;

use std::path::Path;

use dfdu_tree::HostPath;
use serde_json::{Map, Value};

/// Standard base64 with padding.
pub fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = u32::from(c[0]) << 16
            | u32::from(*c.get(1).unwrap_or(&0)) << 8
            | u32::from(*c.get(2).unwrap_or(&0));
        for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            out.push(if i <= c.len() {
                ABC[(n >> shift & 63) as usize] as char
            } else {
                '='
            });
        }
    }
    out
}

/// Adds `key` with the text of `bytes`, and `key_bytes_b64` when they are not UTF-8.
pub fn put_bytes(o: &mut Map<String, Value>, key: &str, bytes: &[u8]) {
    if let Ok(s) = std::str::from_utf8(bytes) {
        o.insert(key.into(), s.into());
    } else {
        o.insert(key.into(), String::from_utf8_lossy(bytes).into());
        o.insert(format!("{key}_bytes_b64"), base64(bytes).into());
    }
}

/// A path on this machine.
pub fn put_path(o: &mut Map<String, Value>, key: &str, path: &Path) {
    put_bytes(o, key, path.as_os_str().as_encoded_bytes());
}

/// A path of a tree, on the host it was scanned on.
pub fn put_host_path(o: &mut Map<String, Value>, key: &str, path: &HostPath) {
    put_bytes(o, key, path.as_bytes());
}

#[cfg(test)]
mod tests;

//! A streaming JSON reader for ncdu exports. It reads bytes, not UTF-8: ncdu writes names as
//! the filesystem has them, so a string can hold any byte but `"` and `\` unescaped. Escapes
//! (`\uXXXX` with surrogate pairs too) become UTF-8. Nothing is kept but the value asked for,
//! and nested values the caller skips are bounded by a depth limit.

use std::io::{self, BufRead};

use crate::ImportError;

/// A JSON number: integers exactly, anything else as a float.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Num {
    U(u64),
    I(i64),
    F(f64),
}

impl Num {
    /// The value as an unsigned integer, if it is one in range (fractions are dropped, as ncdu
    /// does for `mtime`).
    pub fn as_u64(self) -> Option<u64> {
        match self {
            Num::U(u) => Some(u),
            Num::I(_) => None,
            Num::F(f) if f >= 0.0 && f < u64::MAX as f64 => Some(f as u64),
            Num::F(_) => None,
        }
    }
}

pub struct Reader<R> {
    r: R,
    /// Bytes consumed so far, for error messages.
    pos: u64,
}

impl<R: BufRead> Reader<R> {
    pub fn new(r: R) -> Self {
        Reader { r, pos: 0 }
    }

    pub fn error(&self, msg: impl Into<String>) -> ImportError {
        ImportError::Format {
            offset: self.pos,
            msg: msg.into(),
        }
    }

    fn peek_byte(&mut self) -> io::Result<Option<u8>> {
        loop {
            match self.r.fill_buf() {
                Ok(b) => return Ok(b.first().copied()),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
    }

    fn bump(&mut self) {
        self.r.consume(1);
        self.pos += 1;
    }

    fn next_byte(&mut self) -> Result<u8, ImportError> {
        match self.peek_byte()? {
            Some(b) => {
                self.bump();
                Ok(b)
            }
            None => Err(self.error("unexpected end of input")),
        }
    }

    /// The next byte that is not whitespace, without consuming it; `None` at the end.
    pub fn peek(&mut self) -> Result<Option<u8>, ImportError> {
        while let Some(b) = self.peek_byte()? {
            if !matches!(b, b' ' | b'\t' | b'\n' | b'\r') {
                return Ok(Some(b));
            }
            self.bump();
        }
        Ok(None)
    }

    /// Consumes `want`, after whitespace.
    pub fn expect(&mut self, want: u8) -> Result<(), ImportError> {
        match self.peek()? {
            Some(b) if b == want => {
                self.bump();
                Ok(())
            }
            Some(b) => Err(self.error(format!(
                "expected '{}', found '{}'",
                want as char,
                b.escape_ascii()
            ))),
            None => Err(self.error(format!("expected '{}', found the end", want as char))),
        }
    }

    /// After a value in an array or object: true if a `,` followed (consumed), false if
    /// `close` did (consumed too).
    pub fn more(&mut self, close: u8) -> Result<bool, ImportError> {
        match self.peek()? {
            Some(b',') => {
                self.bump();
                Ok(true)
            }
            Some(b) if b == close => {
                self.bump();
                Ok(false)
            }
            _ => Err(self.error(format!("expected ',' or '{}'", close as char))),
        }
    }

    /// A string, at most `max` bytes once unescaped.
    pub fn string(&mut self, max: usize) -> Result<Vec<u8>, ImportError> {
        self.expect(b'"')?;
        let mut out = Vec::new();
        loop {
            // Copy runs of plain bytes straight from the buffer.
            let (n, end) = {
                let buf = loop {
                    match self.r.fill_buf() {
                        Ok(b) => break b,
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        Err(e) => return Err(e.into()),
                    }
                };
                if buf.is_empty() {
                    return Err(self.error("unterminated string"));
                }
                let n = buf
                    .iter()
                    .position(|&b| b == b'"' || b == b'\\')
                    .unwrap_or(buf.len());
                if out.len() + n > max {
                    return Err(ImportError::Limit(format!(
                        "a string longer than {max} bytes"
                    )));
                }
                out.extend_from_slice(&buf[..n]);
                (n, buf.get(n).copied())
            };
            self.r.consume(n);
            self.pos += n as u64;
            match end {
                None => continue,
                Some(b'"') => {
                    self.bump();
                    return Ok(out);
                }
                Some(_) => {
                    self.bump();
                    self.escape(&mut out)?;
                    if out.len() > max {
                        return Err(ImportError::Limit(format!(
                            "a string longer than {max} bytes"
                        )));
                    }
                }
            }
        }
    }

    fn escape(&mut self, out: &mut Vec<u8>) -> Result<(), ImportError> {
        let c = self.next_byte()?;
        let b = match c {
            b'"' => b'"',
            b'\\' => b'\\',
            b'/' => b'/',
            b'b' => 8,
            b'f' => 12,
            b'n' => b'\n',
            b'r' => b'\r',
            b't' => b'\t',
            b'u' => {
                let mut cp = self.hex4()?;
                if (0xD800..0xDC00).contains(&cp) {
                    // A high surrogate; a low one must follow.
                    if self.next_byte()? != b'\\' || self.next_byte()? != b'u' {
                        return Err(self.error("unpaired surrogate"));
                    }
                    let lo = self.hex4()?;
                    if !(0xDC00..0xE000).contains(&lo) {
                        return Err(self.error("unpaired surrogate"));
                    }
                    cp = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                } else if (0xDC00..0xE000).contains(&cp) {
                    return Err(self.error("unpaired surrogate"));
                }
                let ch = char::from_u32(cp).ok_or_else(|| self.error("bad code point"))?;
                let mut buf = [0; 4];
                out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                return Ok(());
            }
            _ => return Err(self.error(format!("bad escape '\\{}'", c.escape_ascii()))),
        };
        out.push(b);
        Ok(())
    }

    fn hex4(&mut self) -> Result<u32, ImportError> {
        let mut v = 0;
        for _ in 0..4 {
            let c = self.next_byte()?;
            let d = (c as char)
                .to_digit(16)
                .ok_or_else(|| self.error("bad \\u escape"))?;
            v = v * 16 + d;
        }
        Ok(v)
    }

    pub fn number(&mut self) -> Result<Num, ImportError> {
        self.peek()?;
        let mut s = String::new();
        while let Some(b) = self.peek_byte()? {
            if !matches!(b, b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E') {
                break;
            }
            if s.len() >= 64 {
                return Err(self.error("number too long"));
            }
            s.push(b as char);
            self.bump();
        }
        if let Ok(u) = s.parse::<u64>() {
            return Ok(Num::U(u));
        }
        if let Ok(i) = s.parse::<i64>() {
            return Ok(Num::I(i));
        }
        match s.parse::<f64>() {
            Ok(f) if f.is_finite() && !s.is_empty() => Ok(Num::F(f)),
            _ => Err(self.error(format!("bad number '{s}'"))),
        }
    }

    /// `true`, `false` or `null` (as `None`).
    pub fn literal(&mut self) -> Result<Option<bool>, ImportError> {
        let (word, v): (&[u8], _) = match self.peek()? {
            Some(b't') => (b"true", Some(true)),
            Some(b'f') => (b"false", Some(false)),
            Some(b'n') => (b"null", None),
            _ => return Err(self.error("expected a value")),
        };
        for &w in word {
            if self.next_byte()? != w {
                return Err(self.error("bad literal"));
            }
        }
        Ok(v)
    }

    /// Reads and drops one value of any kind, nested at most `depth` levels.
    pub fn skip(&mut self, depth: usize) -> Result<(), ImportError> {
        match self.peek()? {
            Some(b'"') => self.string(usize::MAX).map(drop),
            Some(open @ (b'[' | b'{')) => {
                if depth == 0 {
                    return Err(ImportError::Limit("values nested too deeply".into()));
                }
                self.bump();
                let close = if open == b'[' { b']' } else { b'}' };
                if self.peek()? == Some(close) {
                    self.bump();
                    return Ok(());
                }
                loop {
                    if open == b'{' {
                        self.string(usize::MAX)?;
                        self.expect(b':')?;
                    }
                    self.skip(depth - 1)?;
                    if !self.more(close)? {
                        return Ok(());
                    }
                }
            }
            Some(b'-' | b'0'..=b'9') => self.number().map(drop),
            Some(_) => self.literal().map(drop),
            None => Err(self.error("unexpected end of input")),
        }
    }
}

/// Appends `s` as a JSON string. Bytes that are not UTF-8 are written as they are, as ncdu
/// does; the caller counts them.
pub fn write_str(out: &mut Vec<u8>, s: &[u8]) {
    out.push(b'"');
    for &b in s {
        match b {
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\t' => out.extend_from_slice(b"\\t"),
            0..0x20 | 0x7f => {
                out.extend_from_slice(format!("\\u{b:04x}").as_bytes());
            }
            _ => out.push(b),
        }
    }
    out.push(b'"');
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(s: &[u8]) -> Reader<&[u8]> {
        Reader::new(s)
    }

    #[test]
    fn strings_keep_raw_bytes_and_decode_escapes() {
        assert_eq!(r(b"\"a\xffb\"").string(100).unwrap(), b"a\xffb");
        assert_eq!(
            r(br#""\"\\\/\n\u00e9\ud83d\ude00""#).string(100).unwrap(),
            "\"\\/\né😀".as_bytes()
        );
        assert!(r(br#""\ud83d""#).string(100).is_err());
        assert!(r(br#""\ude00x""#).string(100).is_err());
        assert!(matches!(
            r(b"\"abcdef\"").string(3),
            Err(ImportError::Limit(_))
        ));
        assert!(r(b"\"abc").string(100).is_err());
    }

    #[test]
    fn numbers_and_skipping() {
        assert_eq!(r(b"12").number().unwrap(), Num::U(12));
        assert_eq!(r(b"-3").number().unwrap(), Num::I(-3));
        assert_eq!(r(b"1.5e3").number().unwrap().as_u64(), Some(1500));
        assert!(r(b"-").number().is_err());
        let mut x = r(br#"{"a":[1,{"b":null}],"c":"d"} 7"#);
        x.skip(3).unwrap();
        assert_eq!(x.number().unwrap(), Num::U(7));
        assert!(matches!(
            r(b"[[[[1]]]]").skip(3),
            Err(ImportError::Limit(_))
        ));
    }

    #[test]
    fn written_strings_read_back() {
        let name = b"q\"\\\n\x01\x7f\xff\xc3\xa9";
        let mut out = vec![];
        write_str(&mut out, name);
        assert_eq!(r(&out).string(100).unwrap(), name);
    }
}

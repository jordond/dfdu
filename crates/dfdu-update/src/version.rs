//! Release versions: `MAJOR.MINOR.PATCH` with an optional `-PRERELEASE`, ordered as semver
//! orders them (build metadata is not used by dfdu's tags).

use std::cmp::Ordering;
use std::fmt;

/// A release's version, as its tag names it without the `v`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// After the `-`, empty for a release.
    pub pre: String,
}

impl Version {
    /// `0.3.0`, `v0.3.0` or `0.4.0-rc.1`; `None` for anything else.
    pub fn parse(text: &str) -> Option<Version> {
        let text = text.trim();
        let text = text.strip_prefix('v').unwrap_or(text);
        let (core, pre) = match text.split_once('-') {
            Some((core, pre)) if !pre.is_empty() && pre.split('.').all(ident) => (core, pre),
            Some(_) => return None,
            None => (text, ""),
        };
        let mut parts = core.split('.').map(number);
        let v = Version {
            major: parts.next()??,
            minor: parts.next()??,
            patch: parts.next()??,
            pre: pre.to_owned(),
        };
        parts.next().is_none().then_some(v)
    }

    /// The version of this build.
    pub fn current() -> Version {
        Version::parse(crate::CURRENT).unwrap_or(Version {
            major: 0,
            minor: 0,
            patch: 0,
            pre: String::new(),
        })
    }
}

/// A number without a leading zero (`0` itself is fine).
fn number(s: &str) -> Option<u64> {
    if s.is_empty() || (s.len() > 1 && s.starts_with('0')) || !s.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    s.parse().ok()
}

/// A prerelease identifier: letters and digits, and a number written as [`number`] reads it,
/// so two identifiers equal as numbers are equal as text too (`Ord` agrees with `Eq`).
fn ident(s: &str) -> bool {
    if s.bytes().all(|b| b.is_ascii_digit()) {
        return number(s).is_some();
    }
    s.bytes().all(|b| b.is_ascii_alphanumeric())
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                // A release comes after its prereleases.
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => pre_cmp(&self.pre, &other.pre),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Dot-separated identifiers: numbers by value and before words, words as text, and a
/// shorter list before a longer one it starts.
fn pre_cmp(a: &str, b: &str) -> Ordering {
    let mut a = a.split('.');
    let mut b = b.split('.');
    loop {
        match (a.next(), b.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                let o = match (x.parse::<u64>(), y.parse::<u64>()) {
                    (Ok(x), Ok(y)) => x.cmp(&y),
                    (Ok(_), Err(_)) => Ordering::Less,
                    (Err(_), Ok(_)) => Ordering::Greater,
                    (Err(_), Err(_)) => x.cmp(y),
                };
                if o != Ordering::Equal {
                    return o;
                }
            }
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.pre.is_empty() {
            write!(f, "-{}", self.pre)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn versions_read_with_or_without_the_v() {
        assert_eq!(v("0.3.0"), v("v0.3.0"));
        assert_eq!(v(" 1.2.3\n").to_string(), "1.2.3");
        assert_eq!(v("0.4.0-rc.1").to_string(), "0.4.0-rc.1");
        for bad in [
            "",
            "1",
            "1.2",
            "1.2.3.4",
            "01.2.3",
            "1.2.x",
            "1.2.3-",
            "1.2.3-a..b",
            "1.2.3-é",
            "x1.2.3",
            "1.2.3-01",
            "1.2.3-rc.01",
        ] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn versions_order_as_semver_does() {
        let order = [
            "0.2.9",
            "0.3.0-alpha",
            "0.3.0-alpha.1",
            "0.3.0-alpha.beta",
            "0.3.0-beta.2",
            "0.3.0-beta.11",
            "0.3.0-rc.1",
            "0.3.0",
            "0.3.1",
            "0.10.0",
            "1.0.0",
        ];
        for pair in order.windows(2) {
            assert!(v(pair[0]) < v(pair[1]), "{} < {}", pair[0], pair[1]);
        }
    }

    #[test]
    fn this_build_has_a_version() {
        assert_eq!(Version::current().to_string(), crate::CURRENT);
    }
}

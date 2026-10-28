//! Globs over `/`-separated byte paths: `*` matches within one component, `**` across
//! components, `?` one byte other than `/`, and `[...]` is not supported (rules do not use it).
//! Matching is case-sensitive except on Windows and macOS, where file names usually are not.

/// A compiled glob.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Glob {
    pattern: Vec<u8>,
    fold_case: bool,
    shape: Shape,
    /// The literal part before the first component with a wildcard, with its `/`; the whole
    /// pattern when it has no wildcard.
    prefix: Vec<u8>,
    /// See [`Glob::depth`].
    depth: Option<usize>,
}

/// Single-component patterns that need no general matching.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Shape {
    Literal,
    /// `*`
    Any,
    /// `abc*`
    Prefix(Vec<u8>),
    /// `*abc`
    Suffix(Vec<u8>),
    General,
}

fn shape(p: &[u8]) -> Shape {
    let wild = |b: &u8| matches!(b, b'*' | b'?');
    if !p.iter().any(wild) {
        return Shape::Literal;
    }
    if p.contains(&b'/') || p.contains(&b'?') {
        return Shape::General;
    }
    let stars = p.iter().filter(|&&b| b == b'*').count();
    match (stars, p.first(), p.last()) {
        (1, _, _) if p == b"*" => Shape::Any,
        (1, _, Some(b'*')) => Shape::Prefix(p[..p.len() - 1].to_vec()),
        (1, Some(b'*'), _) => Shape::Suffix(p[1..].to_vec()),
        _ => Shape::General,
    }
}

fn literal_prefix(p: &[u8]) -> Vec<u8> {
    if !p.iter().any(|b| matches!(b, b'*' | b'?')) {
        return p.to_vec();
    }
    let mut end = 0;
    for (i, &b) in p.iter().enumerate() {
        if matches!(b, b'*' | b'?') {
            break;
        }
        if b == b'/' {
            end = i + 1;
        }
    }
    p[..end].to_vec()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GlobError {
    Empty,
    /// `**` must be a whole component: `a/**/b`, `**/b`, `a/**`.
    PartialDoubleStar,
    Brackets,
}

impl std::fmt::Display for GlobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            GlobError::Empty => "empty pattern",
            GlobError::PartialDoubleStar => "`**` must be a whole path component",
            GlobError::Brackets => "`[...]` is not supported",
        })
    }
}

impl Glob {
    pub fn new(pattern: &str, fold_case: bool) -> Result<Glob, GlobError> {
        if pattern.is_empty() {
            return Err(GlobError::Empty);
        }
        if pattern.contains('[') {
            return Err(GlobError::Brackets);
        }
        for part in pattern.split('/') {
            if part.contains("**") && part != "**" {
                return Err(GlobError::PartialDoubleStar);
            }
        }
        let pattern = if fold_case {
            pattern.to_ascii_lowercase().into_bytes()
        } else {
            pattern.as_bytes().to_vec()
        };
        let depth = (!pattern.split(|&b| b == b'/').any(|c| c == b"**"))
            .then(|| pattern.iter().filter(|&&b| b == b'/').count() + 1);
        Ok(Glob {
            shape: shape(&pattern),
            prefix: literal_prefix(&pattern),
            depth,
            pattern,
            fold_case,
        })
    }

    /// The pattern as compiled (lower case when case is folded).
    pub fn pattern(&self) -> &[u8] {
        &self.pattern
    }

    /// The literal part before the first component with a wildcard (ending in `/`), or the
    /// whole pattern when it has none. Every path the glob matches starts with it.
    pub fn prefix(&self) -> &[u8] {
        &self.prefix
    }

    /// The number of `/`-separated components every match has; `None` with `**`.
    pub fn depth(&self) -> Option<usize> {
        self.depth
    }

    /// The pattern without any wildcard: a plain name or path, compared directly.
    pub fn literal(&self) -> Option<&[u8]> {
        (!self.pattern.iter().any(|b| matches!(b, b'*' | b'?'))).then_some(&self.pattern)
    }

    pub fn matches(&self, text: &[u8]) -> bool {
        let fold = self.fold_case;
        match &self.shape {
            Shape::Literal => eq(text, &self.pattern, fold),
            Shape::Any => !text.contains(&b'/'),
            Shape::Prefix(p) => starts_with(text, p, fold) && !text.contains(&b'/'),
            Shape::Suffix(s) => {
                text.len() >= s.len()
                    && eq(&text[text.len() - s.len()..], s, fold)
                    && !text.contains(&b'/')
            }
            Shape::General => comps(Some(&self.pattern), Some(text), fold),
        }
    }
}

/// Whether case is folded when names are compared on this platform.
pub fn platform_folds_case() -> bool {
    cfg!(any(windows, target_os = "macos"))
}

/// The first component of `s` and the rest after its `/`, `None` when there is no `/`.
fn split(s: &[u8]) -> (&[u8], Option<&[u8]>) {
    match s.iter().position(|&b| b == b'/') {
        Some(i) => (&s[..i], Some(&s[i + 1..])),
        None => (s, None),
    }
}

/// Matches component by component, without allocating; a `**` component matches zero or more
/// components. `None` is "no components left".
fn comps(p: Option<&[u8]>, t: Option<&[u8]>, fold: bool) -> bool {
    let Some(p) = p else {
        return t.is_none();
    };
    let (pc, p_rest) = split(p);
    if pc == b"**" {
        let mut t = t;
        loop {
            if comps(p_rest, t, fold) {
                return true;
            }
            match t {
                Some(tt) => t = split(tt).1,
                None => return false,
            }
        }
    }
    let Some(t) = t else {
        return false;
    };
    let (tc, t_rest) = split(t);
    component(pc, tc, fold) && comps(p_rest, t_rest, fold)
}

/// One component: `*` any run of bytes, `?` one byte. With `fold`, the pattern is already in
/// lower case and the text is compared as if it were.
fn component(p: &[u8], t: &[u8], fold: bool) -> bool {
    match p.split_first() {
        None => t.is_empty(),
        Some((b'*', rest)) => {
            rest.is_empty() || (0..=t.len()).any(|i| component(rest, &t[i..], fold))
        }
        Some((b'?', rest)) => !t.is_empty() && component(rest, &t[1..], fold),
        Some((&c, rest)) => t.first().is_some_and(|&b| {
            (b == c || (fold && b.to_ascii_lowercase() == c)) && component(rest, &t[1..], fold)
        }),
    }
}

fn eq(a: &[u8], b: &[u8], fold: bool) -> bool {
    a.len() == b.len() && starts_with(a, b, fold)
}

/// `s` starts with `prefix`, ignoring ASCII case when `fold` (`prefix` is in lower case then).
pub fn starts_with(s: &[u8], prefix: &[u8], fold: bool) -> bool {
    s.len() >= prefix.len()
        && s.iter()
            .zip(prefix)
            .all(|(&a, &b)| a == b || (fold && a.to_ascii_lowercase() == b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(p: &str, t: &str) -> bool {
        Glob::new(p, false).expect("glob").matches(t.as_bytes())
    }

    #[test]
    fn stars() {
        assert!(m("*.avd", "pixel.avd"));
        assert!(!m("*.avd", "a/pixel.avd"));
        assert!(m(
            "/h/Library/Developer/Xcode/*OS DeviceSupport/*",
            "/h/Library/Developer/Xcode/iOS DeviceSupport/17.2"
        ));
        assert!(!m("/h/x/*", "/h/x/a/b"));
        assert!(m("?at", "cat"));
        assert!(!m("?at", "/at"));
        assert!(m("*", "anything"));
        assert!(!m("*", "a/b"));
    }

    #[test]
    fn double_stars() {
        assert!(m("**/node_modules/**", "/p/node_modules/x"));
        assert!(m("**/node_modules/**", "/p/node_modules"));
        assert!(m("**/lib/node_modules", "/usr/local/lib/node_modules"));
        assert!(!m("**/lib/node_modules", "/usr/local/lib/node_modules/x"));
        assert!(m("/h/Downloads/**", "/h/Downloads/a/b.dmg"));
        assert!(m("/h/Downloads/**", "/h/Downloads"));
        assert!(!m("/h/Downloads/**", "/h/Downloadsx"));
        assert!(m(
            "**/*.app/**",
            "/Applications/X.app/Contents/node_modules"
        ));
        assert!(m("/a/**/z", "/a/z"));
        assert!(m("/a/**/z", "/a/b/c/z"));
    }

    #[test]
    fn case_folding() {
        let g = Glob::new("*.DMG", true).expect("glob");
        assert!(g.matches(b"Setup.dmg"));
        assert!(
            !Glob::new("*.DMG", false)
                .expect("glob")
                .matches(b"Setup.dmg")
        );
    }

    #[test]
    fn errors_and_literals() {
        assert_eq!(Glob::new("", false), Err(GlobError::Empty));
        assert_eq!(Glob::new("a**", false), Err(GlobError::PartialDoubleStar));
        assert_eq!(Glob::new("[ab]", false), Err(GlobError::Brackets));
        assert_eq!(
            Glob::new("target", false).expect("glob").literal(),
            Some(&b"target"[..])
        );
        assert_eq!(Glob::new("*.x", false).expect("glob").literal(), None);
    }
}

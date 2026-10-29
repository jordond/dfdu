//! What changed between two scans' kept directories.

use super::{SizeMap, Sizes};

/// How one path changed between two scans. A side is `None` where the path was not there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub path: Vec<u8>,
    pub old: Option<Sizes>,
    pub new: Option<Sizes>,
}

impl Change {
    /// The change in disk usage.
    #[must_use]
    pub fn delta(&self) -> i128 {
        self.new.map_or(0, |s| i128::from(s.disk)) - self.old.map_or(0, |s| i128::from(s.disk))
    }

    #[must_use]
    pub fn apparent_delta(&self) -> i128 {
        self.new.map_or(0, |s| i128::from(s.apparent))
            - self.old.map_or(0, |s| i128::from(s.apparent))
    }
}

/// Levels below the root of a relative path (the root is 0).
#[must_use]
pub fn depth(rel: &[u8]) -> usize {
    if rel.is_empty() {
        0
    } else {
        // One more part than separators.
        rel.split(|&b| b == std::path::MAIN_SEPARATOR as u8).count()
    }
}

/// Every path whose sizes differ between `old` and `new`, the biggest change in disk usage
/// first. A path in only one of them is new or gone if it is at most `complete` levels down,
/// where each side has every directory; deeper, it was only left out for being small, so it
/// is not compared (its parents show the change).
#[must_use]
pub fn diff(old: &SizeMap, new: &SizeMap, complete: usize) -> Vec<Change> {
    let mut out = vec![];
    for (path, &o) in old {
        match new.get(path) {
            Some(&n) if n == o => {}
            Some(&n) => out.push(Change {
                path: path.clone(),
                old: Some(o),
                new: Some(n),
            }),
            None if depth(path) <= complete => out.push(Change {
                path: path.clone(),
                old: Some(o),
                new: None,
            }),
            None => {}
        }
    }
    for (path, &n) in new {
        if !old.contains_key(path) && depth(path) <= complete {
            out.push(Change {
                path: path.clone(),
                old: None,
                new: Some(n),
            });
        }
    }
    out.sort_by(|a, b| {
        b.delta()
            .abs()
            .cmp(&a.delta().abs())
            .then_with(|| a.path.cmp(&b.path))
    });
    out
}

#[cfg(test)]
mod tests;

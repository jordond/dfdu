//! The completion of a go-to prompt's last component: the directory it is typed in and what
//! is typed of it ([`Query`]), and the names that complete it ([`complete`]).

use std::io;
use std::path::Path;

use dfdu_tree::HostPath;

use super::{Completion, absolute};

/// Completes the last component of `input` (seen from `current`, as
/// [`resolve`](super::resolve) reads it) from the directories `list` finds in the directory
/// before it. Names starting with `.` are offered only when what was typed of the last
/// component starts with `.`.
pub fn complete(
    input: &str,
    current: &HostPath,
    home: Option<&Path>,
    local: bool,
    list: impl Fn(&HostPath) -> io::Result<Vec<String>>,
) -> Completion {
    let Some(q) = Query::of(input, current, home, local) else {
        return Completion::default();
    };
    match list(&q.dir) {
        Ok(names) => q.finish(names),
        Err(_) => Completion::default(),
    }
}

/// What a completion lists ([`complete`]): the directory before the last component, and the
/// text up to it and of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query {
    /// The directory to list.
    pub dir: HostPath,
    /// The input up to and with its last separator.
    dir_text: String,
    /// What is typed of the last component.
    typed: String,
}

impl Query {
    /// The query of `input`, seen from `current` as [`resolve`](super::resolve) reads it;
    /// `None` when the directory before the last component cannot be resolved.
    #[must_use]
    pub fn of(input: &str, current: &HostPath, home: Option<&Path>, local: bool) -> Option<Self> {
        let host = current.host();
        let cut = input
            .bytes()
            .rposition(|b| host.is_sep(b))
            .map_or(0, |i| i + 1);
        let (dir_text, typed) = input.split_at(cut);
        let dir = if dir_text.is_empty() {
            current.clone()
        } else {
            absolute(dir_text, current, home, local).ok()?
        };
        Some(Query {
            dir,
            dir_text: dir_text.to_owned(),
            typed: typed.to_owned(),
        })
    }

    /// The completion from `names`, the directories in [`Query::dir`].
    #[must_use]
    pub fn finish(&self, names: Vec<String>) -> Completion {
        let (dir_text, typed) = (&self.dir_text, self.typed.as_str());
        let mut found: Vec<String> = (names.into_iter())
            .filter(|n| n.starts_with(typed) && (typed.starts_with('.') || !n.starts_with('.')))
            .collect();
        found.sort();
        found.dedup();
        let sep = char::from(self.dir.host().sep());
        match found.as_slice() {
            [] => Completion::default(),
            [one] => Completion {
                text: Some(format!("{dir_text}{one}{sep}")),
                candidates: Vec::new(),
            },
            [first, rest @ ..] => {
                let mut common = first.len();
                for n in rest {
                    common = common.min(shared(first, n));
                }
                let common = &first[..common];
                Completion {
                    text: (common.len() > typed.len()).then(|| format!("{dir_text}{common}")),
                    candidates: found,
                }
            }
        }
    }
}

/// The length in bytes of the prefix `a` and `b` share, on a character boundary.
fn shared(a: &str, b: &str) -> usize {
    (a.char_indices())
        .zip(b.chars())
        .find(|((_, x), y)| x != y)
        .map_or(a.len().min(b.len()), |((i, _), _)| i)
}

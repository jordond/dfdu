//! Rule locations a scan does not reach because a symbolic link on the way leads out of it. A
//! scan never follows links, so `~/.gradle` linked to another disk is not read by a scan of the
//! home folder, and the Gradle caches in it are only found by scanning where it leads. Only the
//! fixed leading part of each enabled rule's path patterns is looked at, with one `lstat` per
//! component below the scan root; nothing is scanned, and a link's target is read, not followed.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use crate::{RuleIx, RuleSet};

/// A symbolic link below the scan root, on the way to rule locations, that leads out of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Linked {
    /// The link, as its folder names it.
    pub link: PathBuf,
    /// Where it leads: its target joined to its folder, `..` taken off; not followed further.
    pub target: PathBuf,
    /// Indices of the rules whose paths go through it, in rule order.
    pub rules: Vec<RuleIx>,
}

/// The links below `root` that rule path patterns go through to somewhere outside `root`, by
/// link path. Empty where rules do not follow links ([`crate::Env::link_targets`]).
pub fn linked_outside(set: &RuleSet, root: &Path) -> Vec<Linked> {
    if !cfg!(unix) {
        return Vec::new();
    }
    let Ok(root) = std::fs::canonicalize(root) else {
        return Vec::new();
    };
    let fold = set.folds_case();
    let root_text = text(&root, fold);
    let mut found: BTreeMap<PathBuf, Linked> = BTreeMap::new();
    // The walk of each fixed part once: several patterns and rules share them.
    let mut walked: BTreeMap<Vec<u8>, Option<(PathBuf, PathBuf)>> = BTreeMap::new();
    for (i, rule) in set.rules().iter().enumerate() {
        let Ok(ix) = u16::try_from(i).map(RuleIx) else {
            break;
        };
        for g in &rule.paths {
            let fixed = g.prefix();
            let fixed = fixed.strip_suffix(b"/").unwrap_or(fixed);
            let Some(rest) = below(fixed, &root_text) else {
                continue;
            };
            let hit = walked
                .entry(rest.to_vec())
                .or_insert_with(|| walk(&root, &root_text, rest, fold));
            if let Some((link, target)) = hit {
                let l = found.entry(link.clone()).or_insert_with(|| Linked {
                    link: link.clone(),
                    target: target.clone(),
                    rules: Vec::new(),
                });
                if l.rules.last() != Some(&ix) {
                    l.rules.push(ix);
                }
            }
        }
    }
    found.into_values().collect()
}

/// A path as rule patterns spell it: `/`-separated bytes, in lower case when case folds.
fn text(p: &Path, fold: bool) -> Vec<u8> {
    let s = p.to_string_lossy().replace('\\', "/");
    let s = s.trim_end_matches('/');
    if fold {
        s.to_ascii_lowercase().into_bytes()
    } else {
        s.as_bytes().to_vec()
    }
}

/// What `path` has below `root`, without the `/` between; `None` when it is not below it.
fn below<'a>(path: &'a [u8], root: &[u8]) -> Option<&'a [u8]> {
    let rest = path.strip_prefix(root)?;
    match rest.split_first() {
        Some((b'/', rest)) if !rest.is_empty() => Some(rest),
        _ => None,
    }
}

/// Follows `rest` (`/`-separated names) down from `root` with `lstat`, and returns the first
/// link on the way with where it leads, when that is outside `root`. A missing component, or
/// a file, ends the walk; so does a link that stays inside `root`, which the scan reaches
/// where it leads.
fn walk(root: &Path, root_text: &[u8], rest: &[u8], fold: bool) -> Option<(PathBuf, PathBuf)> {
    let mut cur = root.to_path_buf();
    for name in rest.split(|&b| b == b'/').filter(|n| !n.is_empty()) {
        let name = String::from_utf8_lossy(name);
        let next = cur.join(&*name);
        let meta = std::fs::symlink_metadata(&next).ok()?;
        if meta.file_type().is_symlink() {
            let to = std::fs::read_link(&next).ok()?;
            let target = lexical(&cur, &to);
            if below(&text(&target, fold), root_text).is_some() || text(&target, fold) == root_text
            {
                return None;
            }
            // Patterns are in lower case where case folds: the link's own spelling.
            let link = if fold { spelled(&cur, &name) } else { next };
            return Some((link, target));
        }
        if !meta.is_dir() {
            return None;
        }
        cur = next;
    }
    None
}

/// `dir`'s entry named `name` ignoring ASCII case, as the folder spells it.
fn spelled(dir: &Path, name: &str) -> PathBuf {
    let real = std::fs::read_dir(dir).ok().and_then(|d| {
        d.flatten()
            .map(|e| e.file_name())
            .find(|n| n.to_string_lossy().eq_ignore_ascii_case(name))
    });
    match real {
        Some(n) => dir.join(n),
        None => dir.join(name),
    }
}

/// `to` (a link's text) taken from the folder `base`, with `.` and `..` resolved by name.
fn lexical(base: &Path, to: &Path) -> PathBuf {
    let mut out = if to.is_absolute() {
        PathBuf::new()
    } else {
        base.to_path_buf()
    };
    for c in to.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c.as_os_str()),
        }
    }
    out
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::{Env, Platform, Source, load_from};

    const RULES: &str = r#"
schema_version = 1
[[rule]]
id = "test.gradle"
category = "dev.pkgcache"
name = "Gradle caches"
safety = "regenerable"
explanation = "x"
lost = "y"
[rule.match]
kind = "dir"
paths = ["{home}/.gradle/caches", "{home}/.gradle/wrapper/dists"]

[[rule]]
id = "test.tools"
category = "dev.pkgcache"
name = "Tool caches"
safety = "safe"
explanation = "x"
lost = "y"
[rule.match]
kind = "dir"
paths = ["{home}/.tool/cache/*", "{home}/.inside/cache", "{home}/plain/cache", "{home}/.gradle/other"]
"#;

    /// A link to another folder outside the root is reported once, with every rule through
    /// it; one that stays inside, a plain folder and a missing one are not.
    #[test]
    fn links_out_of_the_root_on_the_way_to_rule_paths() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let top = std::fs::canonicalize(tmp.path()).expect("canonical");
        let home = top.join("home");
        for d in ["home/real", "home/plain/cache", "data/gradle", "data/tool"] {
            std::fs::create_dir_all(top.join(d)).expect("mkdir");
        }
        let link = std::os::unix::fs::symlink;
        link(top.join("data/gradle"), home.join(".gradle")).expect("link");
        link(PathBuf::from("../data/tool"), home.join(".tool")).expect("link");
        link(home.join("real"), home.join(".inside")).expect("link");
        let env = Env::with(Platform::Linux, &[("HOME", home.to_str().expect("utf-8"))]);
        let (set, errors) = load_from(&env, &[(Source::File("/r.toml".into()), RULES.into())]);
        assert!(errors.is_empty(), "{errors:#?}");
        let ix = |id: &str| set.index(id).expect(id);
        let got = linked_outside(&set, &home);
        assert_eq!(
            got,
            [
                Linked {
                    link: home.join(".gradle"),
                    target: top.join("data/gradle"),
                    rules: vec![ix("test.gradle"), ix("test.tools")],
                },
                Linked {
                    link: home.join(".tool"),
                    target: top.join("data/tool"),
                    rules: vec![ix("test.tools")],
                },
            ]
        );
        // From the top, both links lead inside the scan.
        assert_eq!(linked_outside(&set, &top), []);
    }

    #[test]
    fn link_text_is_resolved_by_name() {
        let b = Path::new("/h/u");
        assert_eq!(lexical(b, Path::new("../d/./x")), Path::new("/h/d/x"));
        assert_eq!(lexical(b, Path::new("/m/x/../y")), Path::new("/m/y"));
        assert_eq!(below(b"/h/u/.g", b"/h/u"), Some(&b".g"[..]));
        assert_eq!(below(b"/h/uu/.g", b"/h/u"), None);
        assert_eq!(below(b"/h/u", b"/h/u"), None);
    }
}

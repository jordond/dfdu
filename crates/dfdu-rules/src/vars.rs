//! Path variables in rule patterns: `{home}`, `{cache}`, `{config}`, `{data}`, `{local_data}`,
//! `{temp}`, `{downloads}`, `{program_files}` and `{env:NAME|fallback}` (the fallback may hold
//! variables itself). Values come from an [`Env`], so tests do not depend on the machine.
//! `{program_files}` has two values on Linux and Windows; a pattern with it expands to one
//! pattern per value.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use crate::Platform;

/// A pattern whose variables cannot be expanded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExpandError {
    /// A `{` without its `}`, in this pattern.
    Unclosed(String),
    /// A variable name that is neither a directory variable nor a match variable.
    Unknown(String),
}

impl std::fmt::Display for ExpandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExpandError::Unclosed(s) => write!(f, "unclosed `{{` in `{s}`"),
            ExpandError::Unknown(name) => write!(f, "unknown variable `{{{name}}}`"),
        }
    }
}

impl std::error::Error for ExpandError {}

/// Where variables get their values: the platform and the environment.
#[derive(Clone, Debug)]
pub struct Env {
    pub platform: Platform,
    vars: HashMap<String, String>,
    /// Whether path patterns also match where a symbolic link in their fixed part leads
    /// ([`Env::link_targets`]).
    links: Links,
}

/// Where [`Env::link_targets`] learns where links lead.
#[derive(Clone, Debug)]
enum Links {
    /// Patterns are taken as they are.
    Off,
    /// From this machine's disks; every pattern that leads elsewhere is recorded, by
    /// pattern, for [`Env::links_followed`].
    Resolve(Arc<Mutex<BTreeMap<String, String>>>),
    /// From another machine's record of them ([`Env::with_links`]), never from this one's
    /// disks.
    Given(Arc<BTreeMap<String, String>>),
}

impl Env {
    /// This machine's platform and environment. Directories are resolved the way scan roots
    /// are (on macOS `TMPDIR` is under `/var`, a symlink to `/private/var`).
    #[must_use]
    pub fn current() -> Env {
        let vars = std::env::vars()
            .map(|(k, v)| {
                let dir = k == "HOME" || k == "TMPDIR" || k.ends_with("_HOME");
                match std::fs::canonicalize(&v) {
                    Ok(p) if dir && cfg!(unix) => (k, p.to_string_lossy().into_owned()),
                    _ => (k, v),
                }
            })
            .collect();
        Env {
            platform: Platform::current(),
            vars,
            links: if cfg!(unix) {
                Links::Resolve(Arc::default())
            } else {
                Links::Off
            },
        }
    }

    /// `platform` with exactly the variables in `vars`.
    #[must_use]
    pub fn with(platform: Platform, vars: &[(&str, &str)]) -> Env {
        Env {
            platform,
            vars: vars
                .iter()
                .map(|&(k, v)| (k.to_owned(), v.to_owned()))
                .collect(),
            links: Links::Off,
        }
    }

    /// This environment with path patterns following the links on this machine's disks, as
    /// [`Env::current`] does.
    #[must_use]
    pub fn resolving_links(mut self) -> Env {
        self.links = Links::Resolve(Arc::default());
        self
    }

    /// This environment with path patterns leading where `followed` (another machine's
    /// [`Env::links_followed`]) says, and nowhere else: links on this machine's disks are
    /// never looked at. A remote agent's rules, rebuilt on the local side, so match alike.
    #[must_use]
    pub fn with_links(mut self, followed: &[(String, String)]) -> Env {
        self.links = Links::Given(Arc::new(followed.iter().cloned().collect()));
        self
    }

    /// The path patterns that led elsewhere through a link so far, with where they lead, in
    /// pattern order: what [`Env::with_links`] takes to expand them the same way.
    #[must_use]
    pub fn links_followed(&self) -> Vec<(String, String)> {
        match &self.links {
            Links::Off => Vec::new(),
            Links::Resolve(seen) => seen
                .lock()
                .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default(),
            Links::Given(m) => m.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        }
    }

    /// The expanded path pattern `pattern`, and, when a symbolic link in its fixed leading
    /// part (before the first wildcard) leads elsewhere, the pattern there. The scan does not
    /// follow links, so a tool's folder that is one (`~/.gradle` to `/mnt/data/gradle`) is
    /// only seen by scanning where it leads, and is matched there.
    #[must_use]
    pub fn link_targets(&self, pattern: String) -> Vec<String> {
        let seen = match &self.links {
            Links::Off => return vec![pattern],
            Links::Given(m) => {
                let there = m.get(&pattern).cloned();
                return std::iter::once(pattern).chain(there).collect();
            }
            Links::Resolve(seen) => seen,
        };
        if !pattern.starts_with('/') {
            return vec![pattern];
        }
        let fixed = match pattern.find(['*', '?']) {
            Some(w) => &pattern[..pattern[..w].rfind('/').unwrap_or(0)],
            None => pattern.as_str(),
        };
        // The longest part of it that is there.
        let mut have = fixed;
        let real = loop {
            if have.is_empty() {
                return vec![pattern];
            }
            if let Ok(p) = std::fs::canonicalize(have) {
                break slashes(&p.to_string_lossy());
            }
            have = &have[..have.rfind('/').unwrap_or(0)];
        };
        let same = if crate::glob::platform_folds_case(self.platform) {
            real.eq_ignore_ascii_case(have)
        } else {
            real == have
        };
        if same || real.is_empty() {
            return vec![pattern];
        }
        let there = format!("{real}{}", &pattern[have.len()..]);
        if let Ok(mut seen) = seen.lock() {
            seen.insert(pattern.clone(), there.clone());
        }
        vec![pattern, there]
    }

    /// The variables rules written as `texts` can read here: the ones behind the named
    /// directories and every `{env:NAME}`, with their values. `Env::with` of these and the
    /// same platform expands those rules as this environment does (a remote agent's rules,
    /// rebuilt on the local side).
    #[must_use]
    pub fn used_by(&self, texts: &[&str]) -> Vec<(String, String)> {
        const NAMED: [&str; 11] = [
            "HOME",
            "USERPROFILE",
            "XDG_CACHE_HOME",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "LOCALAPPDATA",
            "APPDATA",
            "TMPDIR",
            "TEMP",
            "ProgramFiles",
            "ProgramFiles(x86)",
        ];
        let mut names: Vec<&str> = NAMED.to_vec();
        for t in texts {
            for (i, _) in t.match_indices("env:") {
                let rest = &t[i + 4..];
                let end = rest.find(['|', '}']).unwrap_or(rest.len());
                if !names.contains(&&rest[..end]) {
                    names.push(&rest[..end]);
                }
            }
        }
        let mut out: Vec<(String, String)> = names
            .into_iter()
            .filter_map(|n| self.vars.get(n).map(|v| (n.to_owned(), v.clone())))
            .collect();
        out.sort();
        out
    }

    fn var(&self, name: &str) -> Option<&str> {
        self.vars
            .get(name)
            .map(String::as_str)
            .filter(|v| !v.is_empty())
    }

    /// The home directory, with `/` separators.
    #[must_use]
    pub fn home(&self) -> Option<String> {
        let h = match self.platform {
            Platform::Windows => self.var("USERPROFILE"),
            _ => self.var("HOME"),
        }?;
        Some(slashes(h))
    }

    /// Values of a named variable, empty when it has none here (no `HOME`); `None` for a name
    /// that is not a variable.
    fn named(&self, name: &str) -> Option<Vec<String>> {
        use Platform::{Linux, MacOs, Windows};
        let home = self.home();
        let under_home = |rel: &str| home.as_ref().map(|h| format!("{h}/{rel}"));
        let xdg = |var: &str, rel: &str| self.var(var).map(slashes).or_else(|| under_home(rel));
        let p = self.platform;
        let one = |v: Option<String>| Some(v.into_iter().collect::<Vec<_>>());
        match name {
            "home" => one(home.clone()),
            "cache" => one(match p {
                MacOs => under_home("Library/Caches"),
                Linux => xdg("XDG_CACHE_HOME", ".cache"),
                Windows => self.var("LOCALAPPDATA").map(slashes),
            }),
            "config" => one(match p {
                MacOs => under_home("Library/Application Support"),
                Linux => xdg("XDG_CONFIG_HOME", ".config"),
                Windows => self.var("APPDATA").map(slashes),
            }),
            "data" => one(match p {
                MacOs => under_home("Library/Application Support"),
                Linux => xdg("XDG_DATA_HOME", ".local/share"),
                Windows => self.var("APPDATA").map(slashes),
            }),
            "local_data" => one(match p {
                MacOs => under_home("Library/Application Support"),
                Linux => xdg("XDG_DATA_HOME", ".local/share"),
                Windows => self.var("LOCALAPPDATA").map(slashes),
            }),
            "temp" => one(match p {
                MacOs => self.var("TMPDIR").map(slashes),
                Linux => self
                    .var("TMPDIR")
                    .map(slashes)
                    .or_else(|| Some("/tmp".into())),
                Windows => self.var("TEMP").map(slashes),
            }),
            // The XDG user dir and the Windows known folder are the defaults on nearly every
            // system; reading `user-dirs.dirs` or the registry is left for later.
            "downloads" => one(under_home("Downloads")),
            "program_files" => Some(match p {
                MacOs => vec!["/Applications".into()],
                Linux => vec!["/opt".into(), "/usr".into()],
                Windows => ["ProgramFiles", "ProgramFiles(x86)"]
                    .iter()
                    .filter_map(|v| self.var(v).map(slashes))
                    .collect(),
            }),
            _ => None,
        }
    }

    /// Expands every variable in `s`. One result per combination of multi-valued variables;
    /// none when a variable has no value here, so a rule for a missing directory matches
    /// nothing.
    ///
    /// # Errors
    ///
    /// `s` names an unknown variable or has an unclosed `{`.
    pub fn expand(&self, s: &str) -> Result<Vec<String>, ExpandError> {
        let Some(open) = s.find('{') else {
            return Ok(vec![s.to_owned()]);
        };
        let close = matching_brace(s, open).ok_or_else(|| ExpandError::Unclosed(s.to_owned()))?;
        let inner = &s[open + 1..close];
        let values = if let Some(env) = inner.strip_prefix("env:") {
            let (name, fallback) = env.split_once('|').unwrap_or((env, ""));
            match self.var(name) {
                Some(v) => vec![slashes(v)],
                None if fallback.is_empty() => Vec::new(),
                None => self.expand(fallback)?,
            }
        } else if is_match_variable(inner) {
            // Filled in per match by the caller (`{name}`, `{parent}`, ...): left as is.
            vec![format!("{{{inner}}}")]
        } else {
            self.named(inner)
                .ok_or_else(|| ExpandError::Unknown(inner.to_owned()))?
        };
        let (head, tail) = (&s[..open], &s[close + 1..]);
        let mut out = Vec::new();
        for rest in self.expand(tail)? {
            for v in &values {
                out.push(format!("{head}{v}{rest}"));
            }
        }
        Ok(out)
    }
}

/// Variables that stand for the matched entry, not for a directory of the platform.
fn is_match_variable(name: &str) -> bool {
    matches!(name, "path" | "parent" | "name" | "name_stem" | "item_id")
}

fn matching_brace(s: &str, open: usize) -> Option<usize> {
    let mut depth = 0;
    for (i, c) in s[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + i);
                }
            }
            _ => {}
        }
    }
    None
}

fn slashes(s: &str) -> String {
    s.replace('\\', "/").trim_end_matches('/').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What [`Env::expand`] returns for a pattern that can match nothing here.
    const NOTHING: [String; 0] = [];

    #[test]
    fn used_by_names_the_variables_rules_can_read() {
        let e = Env::with(
            Platform::Linux,
            &[
                ("HOME", "/h"),
                ("GOPATH", "/g"),
                ("SECRET", "x"),
                ("A", "/a"),
            ],
        );
        let rules = "paths = [\"{env:GOPATH}/pkg\", \"{env:NONE|{env:A}}/b\"]";
        assert_eq!(
            e.used_by(&[rules]),
            [("A", "/a"), ("GOPATH", "/g"), ("HOME", "/h")]
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
        );
    }

    fn mac() -> Env {
        Env::with(
            Platform::MacOs,
            &[("HOME", "/Users/u"), ("TMPDIR", "/var/folders/x/T/")],
        )
    }

    #[test]
    fn named_variables() {
        let e = mac();
        assert_eq!(
            e.expand("{cache}/pip").unwrap(),
            ["/Users/u/Library/Caches/pip"]
        );
        assert_eq!(e.expand("{temp}").unwrap(), ["/var/folders/x/T"]);
        let linux = Env::with(
            Platform::Linux,
            &[("HOME", "/home/u"), ("XDG_CACHE_HOME", "/c")],
        );
        assert_eq!(linux.expand("{cache}/x").unwrap(), ["/c/x"]);
        assert_eq!(linux.expand("{config}").unwrap(), ["/home/u/.config"]);
        assert_eq!(
            linux.expand("{program_files}/**").unwrap(),
            ["/opt/**", "/usr/**"]
        );
        let win = Env::with(
            Platform::Windows,
            &[
                ("USERPROFILE", r"C:\Users\u"),
                ("LOCALAPPDATA", r"C:\Users\u\AppData\Local"),
            ],
        );
        assert_eq!(
            win.expand("{cache}/Temp").unwrap(),
            ["C:/Users/u/AppData/Local/Temp"]
        );
        // No ProgramFiles in this environment: the pattern matches nothing.
        assert_eq!(win.expand("{program_files}/x").unwrap(), NOTHING);
    }

    #[test]
    fn env_with_nested_fallback() {
        let e = mac();
        let p = "{env:ANDROID_AVD_HOME|{env:ANDROID_USER_HOME|{home}/.android}/avd}/*.avd";
        assert_eq!(e.expand(p).unwrap(), ["/Users/u/.android/avd/*.avd"]);
        let e = Env::with(
            Platform::Linux,
            &[("HOME", "/h"), ("ANDROID_USER_HOME", "/a")],
        );
        assert_eq!(e.expand(p).unwrap(), ["/a/avd/*.avd"]);
        assert_eq!(e.expand("{env:NOPE}/x").unwrap(), NOTHING);
    }

    #[test]
    fn match_variables_stay_and_errors_are_reported() {
        assert_eq!(
            mac().expand("{name_stem}.ini").unwrap(),
            ["{name_stem}.ini"]
        );
        let unknown = mac().expand("{nope}").unwrap_err();
        assert_eq!(unknown, ExpandError::Unknown("nope".into()));
        assert_eq!(unknown.to_string(), "unknown variable `{nope}`");
        let unclosed = mac().expand("{home").unwrap_err();
        assert_eq!(unclosed.to_string(), "unclosed `{` in `{home`");
        // No home directory: nothing below it can match.
        assert_eq!(
            Env::with(Platform::Linux, &[]).expand("{home}/x").unwrap(),
            NOTHING
        );
    }

    #[cfg(unix)]
    #[test]
    fn link_targets_follow_links_in_the_fixed_part() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let r = root.to_str().unwrap();
        std::fs::create_dir_all(root.join("data/avd")).unwrap();
        std::fs::create_dir_all(root.join("home")).unwrap();
        std::os::unix::fs::symlink(root.join("data"), root.join("home/.android")).unwrap();
        let e = Env::with(Platform::Linux, &[]).resolving_links();
        // Through the link, also when what follows it is not there (yet).
        let p = format!("{r}/home/.android/avd/*.avd");
        assert_eq!(
            e.link_targets(p.clone()),
            [p, format!("{r}/data/avd/*.avd")]
        );
        let p = format!("{r}/home/.android/nope/x");
        assert_eq!(e.link_targets(p.clone()), [p, format!("{r}/data/nope/x")]);
        // No link, a pattern that is not anchored, or links not followed: as it is.
        let p = format!("{r}/home/x/**");
        assert_eq!(e.link_targets(p.clone()), [p]);
        assert_eq!(e.link_targets("**/lib".into()), ["**/lib"]);
        let p = format!("{r}/home/.android/avd");
        assert_eq!(Env::with(Platform::Linux, &[]).link_targets(p.clone()), [p]);
    }

    /// What one machine followed, another expands alike without looking at its own disks.
    #[cfg(unix)]
    #[test]
    fn links_followed_expand_alike_elsewhere() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let r = root.to_str().unwrap();
        std::fs::create_dir_all(root.join("data")).unwrap();
        std::os::unix::fs::symlink(root.join("data"), root.join("tool")).unwrap();
        let here = Env::with(Platform::Linux, &[]).resolving_links();
        let p = format!("{r}/tool/cache");
        let both = here.link_targets(p.clone());
        assert_eq!(both, [p.clone(), format!("{r}/data/cache")]);
        assert_eq!(
            here.link_targets(format!("{r}/plain")),
            [format!("{r}/plain")]
        );
        let followed = here.links_followed();
        assert_eq!(followed, [(p.clone(), format!("{r}/data/cache"))]);
        // The link is gone; the record still leads where it did.
        std::fs::remove_file(root.join("tool")).unwrap();
        let there = Env::with(Platform::Linux, &[]).with_links(&followed);
        assert_eq!(there.link_targets(p.clone()), both);
        assert_eq!(
            there.link_targets(format!("{r}/plain")),
            [format!("{r}/plain")]
        );
        assert_eq!(there.links_followed(), followed);
        assert_eq!(Env::with(Platform::Linux, &[]).links_followed(), []);
    }
}

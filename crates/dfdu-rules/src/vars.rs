//! Path variables in rule patterns: `{home}`, `{cache}`, `{config}`, `{data}`, `{local_data}`,
//! `{temp}`, `{downloads}`, `{program_files}` and `{env:NAME|fallback}` (the fallback may hold
//! variables itself). Values come from an [`Env`], so tests do not depend on the machine.
//! `{program_files}` has two values on Linux and Windows; a pattern with it expands to one
//! pattern per value.

use std::collections::HashMap;

use crate::Platform;

/// Where variables get their values: the platform and the environment.
#[derive(Clone, Debug)]
pub struct Env {
    pub platform: Platform,
    vars: HashMap<String, String>,
}

impl Env {
    /// This machine's platform and environment.
    pub fn current() -> Env {
        Env {
            platform: Platform::current(),
            vars: std::env::vars().collect(),
        }
    }

    /// `platform` with exactly the variables in `vars`.
    pub fn with(platform: Platform, vars: &[(&str, &str)]) -> Env {
        Env {
            platform,
            vars: vars
                .iter()
                .map(|&(k, v)| (k.to_owned(), v.to_owned()))
                .collect(),
        }
    }

    fn var(&self, name: &str) -> Option<&str> {
        self.vars
            .get(name)
            .map(String::as_str)
            .filter(|v| !v.is_empty())
    }

    /// The home directory, with `/` separators.
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
        use Platform::*;
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
    /// nothing. An unknown variable or an unclosed `{` is an error.
    pub fn expand(&self, s: &str) -> Result<Vec<String>, String> {
        let Some(open) = s.find('{') else {
            return Ok(vec![s.to_owned()]);
        };
        let close = matching_brace(s, open).ok_or_else(|| format!("unclosed `{{` in `{s}`"))?;
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
                .ok_or_else(|| format!("unknown variable `{{{inner}}}`"))?
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
        assert!(win.expand("{program_files}/x").unwrap().is_empty());
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
        assert!(e.expand("{env:NOPE}/x").unwrap().is_empty());
    }

    #[test]
    fn match_variables_stay_and_errors_are_reported() {
        assert_eq!(
            mac().expand("{name_stem}.ini").unwrap(),
            ["{name_stem}.ini"]
        );
        assert!(
            mac()
                .expand("{nope}")
                .unwrap_err()
                .contains("unknown variable")
        );
        assert!(mac().expand("{home").unwrap_err().contains("unclosed"));
        // No home directory: nothing below it can match.
        assert!(
            Env::with(Platform::Linux, &[])
                .expand("{home}/x")
                .unwrap()
                .is_empty()
        );
    }
}

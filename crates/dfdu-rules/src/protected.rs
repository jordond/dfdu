//! Protected paths: never offered for deletion, whatever the
//! rules say. Fixed in the code; rule files cannot change them. A protected entry protects
//! everything below it, except where an exception says otherwise (`/usr/local` below `/usr`).
//!
//! Conditions that need more than the path (files tracked by version control, database data
//! directories found by their marker files) are checked by the classifier, not here.

use crate::hash::FastSet as HashSet;

use crate::vars::Env;
use crate::{Glob, Platform, glob};

/// Names protected wherever they are.
const NAMES: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".jj",
    ".ssh",
    ".gnupg",
    ".aws",
    ".kube",
    ".password-store",
    ".netrc",
    ".npmrc",
    ".pypirc",
    "*.kdbx",
    "*.pem",
    "*.key",
    "*.p12",
    "id_rsa*",
    "id_ed25519*",
    "wallet.dat",
    ".env",
    ".env.*",
    "lost+found",
];

/// Paths protected on every platform (after variable expansion).
const ALL: &[&str] = &[
    "{config}/gcloud",
    "{home}/.docker/config.json",
    "{env:CARGO_HOME|{home}/.cargo}/bin",
    "{env:CARGO_HOME|{home}/.cargo}/config.toml",
    "{env:CARGO_HOME|{home}/.cargo}/credentials.toml",
    "{env:CARGO_HOME|{home}/.cargo}/.crates.toml",
    "{env:CARGO_HOME|{home}/.cargo}/.crates2.json",
    "{home}/.m2/settings.xml",
    "{env:GRADLE_USER_HOME|{home}/.gradle}/gradle.properties",
    "{env:GRADLE_USER_HOME|{home}/.gradle}/init.d",
    "{config}/dfdu",
];

const MACOS: &[&str] = &[
    "/System",
    "/usr",
    "/bin",
    "/sbin",
    "/dev",
    "/private/etc",
    "/private/var/db",
    // `/etc` and `/var` are symlinks into `/private`; paths may arrive uncanonicalized.
    "/etc",
    "/var/db",
    "/Library/Apple",
    "/private/var/folders",
    "/Library/Developer/CoreSimulator/Volumes",
    "/Library/Developer/CoreSimulator/Images",
    "{home}/Library/Mobile Documents",
    "{home}/Library/CloudStorage",
    "{home}/Library/Keychains",
    "{home}/Library/Preferences",
    "{home}/Library/Mail",
    "{home}/Library/Messages",
    "{home}/Library/Containers",
    "{home}/Library/Group Containers",
    "{home}/Library/Application Support/MobileSync",
    "**/*.photoslibrary",
    "**/*.musiclibrary",
    "**/*.tvlibrary",
    "**/*.fcpbundle",
    "**/*.logicx",
    "**/*.lrdata",
    "**/*.lrcat",
    "**/.Spotlight-V100",
    "**/.fseventsd",
    "**/.DocumentRevisions-V100",
    "**/.MobileBackups",
    "**/.TemporaryItems",
    "**/Backups.backupdb",
    "**/*.backupbundle",
    "**/*.sparsebundle",
    "**/*.app/**",
    "/Users/*",
];

const MACOS_EXCEPT: &[&str] = &[
    "/usr/local",
    "{home}",
    "/Users/Shared",
    "{temp}",
    // Cache subdirectories inside app containers are named by rules.
    "{home}/Library/Containers/*/Data/Library/Caches",
    "{home}/Library/Group Containers/*/Library/Caches",
    // Reported with Docker's own prune command (`docker.desktop-disk`), never deleted.
    "{home}/Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw",
];

const LINUX: &[&str] = &[
    "/bin",
    "/boot",
    "/dev",
    "/etc",
    "/lib*",
    "/proc",
    "/run",
    "/sbin",
    "/srv",
    "/sys",
    "/usr",
    "/var/lib",
    "/nix/store",
    "/snap",
    "/opt",
    "{data}/keyrings",
    "{data}/kwalletd",
    "{home}/.mozilla/firefox/*",
    "{home}/.thunderbird",
    "{data}/Steam/steamapps/common",
    "{data}/Steam/steamapps/compatdata",
    "/home/*",
];

const LINUX_EXCEPT: &[&str] = &["{home}", "{home}/.mozilla/firefox/*/cache2"];

const WINDOWS: &[&str] = &[
    "C:/Windows",
    "C:/Program Files",
    "C:/Program Files (x86)",
    "C:/ProgramData",
    "C:/Recovery",
    "C:/System Volume Information",
    "C:/$Recycle.Bin",
    "*:/pagefile.sys",
    "*:/swapfile.sys",
    "*:/hiberfil.sys",
    "{home}/NTUSER.DAT*",
    "{config}/Microsoft/Credentials",
    "{config}/Microsoft/Protect",
    "{cache}/Microsoft/Credentials",
    "{home}/OneDrive*",
    "{home}/Dropbox",
    "{home}/Google Drive",
    "**/ext4.vhdx",
    "{cache}/Packages",
    "C:/Users/*",
];

const WINDOWS_EXCEPT: &[&str] = &[
    "C:/Windows/Temp",
    "C:/ProgramData/Microsoft/Windows/WER",
    "{home}",
    "{cache}/Packages/*/LocalCache",
    "{cache}/Packages/*/AC/INetCache",
];

/// Whether an entry is protected, as passed from a directory to its entries.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum State {
    #[default]
    Off,
    /// By a protected path; an exception below can lift it.
    On,
    /// By a protected name; nothing below it is ever excepted.
    Sticky,
}

impl State {
    #[must_use]
    pub fn protected(self) -> bool {
        self != State::Off
    }
}

/// What [`Protected::dir`] found for one directory.
#[derive(Clone, Debug, Default)]
pub struct Dir {
    path_names: HashSet<Vec<u8>>,
    path_globs: Vec<usize>,
    except_names: HashSet<Vec<u8>>,
    except_globs: Vec<usize>,
}

/// The protected paths and exceptions that can match at or below a directory's entries
/// ([`crate::Reach`]).
#[derive(Clone, Debug, Default)]
pub struct Reach {
    paths: SetReach,
    except: SetReach,
}

/// Indices into a [`Set`]'s literal list and globs.
#[derive(Clone, Debug, Default)]
struct SetReach {
    literals: Vec<u32>,
    globs: Vec<u32>,
}

/// Globs split for speed: literal ones in a set (by their lower-case form when names fold
/// case), the others tried one by one.
#[derive(Clone, Debug, Default)]
struct Set {
    literal: HashSet<Vec<u8>>,
    /// `literal` in a list, for [`Reach`].
    literal_list: Vec<Vec<u8>>,
    globs: Vec<Glob>,
    fold: bool,
}

impl Set {
    fn add(&mut self, g: Glob) {
        match g.literal() {
            Some(l) => {
                if self.literal.insert(l.to_vec()) {
                    self.literal_list.push(l.to_vec());
                }
            }
            None => self.globs.push(g),
        }
    }

    fn matches(&self, s: &[u8]) -> bool {
        let hit = if self.fold && s.iter().any(u8::is_ascii_uppercase) {
            self.literal.contains(&s.to_ascii_lowercase())
        } else {
            self.literal.contains(s)
        };
        hit || self.globs.iter().any(|g| g.matches(s))
    }
}

/// The protected paths for one platform and environment.
#[derive(Clone, Debug)]
pub struct Protected {
    names: Set,
    /// Each protects the path it matches and everything below.
    paths: Set,
    /// Paths (and what is below them) that `paths` does not protect after all.
    except: Set,
}

impl Protected {
    /// The protected names and paths for `env`'s platform, with its variables expanded.
    ///
    /// # Panics
    ///
    /// A built-in protected pattern names an unknown variable or is not a valid glob; the
    /// lists are constants and the tests build them for every platform.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "the protected lists are constants; a bad entry is a bug the tests catch"
    )]
    pub fn new(env: &Env) -> Protected {
        let fold = glob::platform_folds_case(env.platform);
        let (paths, except) = match env.platform {
            Platform::MacOs => (MACOS, MACOS_EXCEPT),
            Platform::Linux => (LINUX, LINUX_EXCEPT),
            Platform::Windows => (WINDOWS, WINDOWS_EXCEPT),
        };
        let mut p = Protected {
            names: Set {
                fold,
                ..Set::default()
            },
            paths: Set {
                fold,
                ..Set::default()
            },
            except: Set {
                fold,
                ..Set::default()
            },
        };
        let expand = |list: &[&str]| -> Vec<String> {
            list.iter()
                .flat_map(|s| env.expand(s).expect("protected path variables"))
                .collect()
        };
        let glob = |s: &str| Glob::new(s, fold).expect("protected path glob");
        for n in NAMES {
            p.names.add(glob(n));
        }
        for path in expand(ALL).into_iter().chain(expand(paths)) {
            // `**/NAME` and `**/NAME/**` protect a name wherever it is.
            match path.strip_prefix("**/") {
                Some(name) => p.names.add(glob(name.trim_end_matches("/**"))),
                None => p.paths.add(glob(&path)),
            }
        }
        for path in expand(except) {
            p.except.add(glob(&path));
        }
        p
    }

    /// What can protect the entries of the directory `dir` (its path with a `/` at the end):
    /// the literal paths that are its entries, by name, and the globs that can match there.
    #[must_use]
    pub fn dir(&self, dir: &[u8]) -> Dir {
        self.dir_in(dir, None).0
    }

    /// [`Protected::dir`] trying only the paths in `reach` (all with `None`), and the paths
    /// that can reach the directory's subdirectories.
    #[must_use]
    pub fn dir_in(&self, dir: &[u8], reach: Option<&Reach>) -> (Dir, Reach) {
        let fold = self.paths.fold;
        let depth = crate::matcher::depth_below(dir);
        let folded = if fold {
            dir.to_ascii_lowercase()
        } else {
            dir.to_vec()
        };
        #[expect(
            clippy::cast_possible_truncation,
            reason = "a set holds the constant protected patterns, a few hundred at most"
        )]
        let pick = |set: &Set, r: Option<&SetReach>| {
            let mut names = HashSet::default();
            let mut below = SetReach::default();
            let lits: Box<dyn Iterator<Item = u32>> = match r {
                Some(r) => Box::new(r.literals.iter().copied()),
                None => Box::new(0..set.literal_list.len() as u32),
            };
            for k in lits {
                let Some(rest) = set.literal_list[k as usize].strip_prefix(&folded[..]) else {
                    continue;
                };
                if rest.is_empty() {
                    continue;
                }
                below.literals.push(k);
                if !rest.contains(&b'/') {
                    names.insert(rest.to_vec());
                }
            }
            let globs: Box<dyn Iterator<Item = u32>> = match r {
                Some(r) => Box::new(r.globs.iter().copied()),
                None => Box::new(0..set.globs.len() as u32),
            };
            let mut here = Vec::new();
            for k in globs {
                let g = &set.globs[k as usize];
                let (now, deeper) = crate::matcher::may_match_below(g, dir, depth, fold);
                if now {
                    here.push(k as usize);
                }
                if deeper {
                    below.globs.push(k);
                }
            }
            (names, here, below)
        };
        let (path_names, path_globs, paths) = pick(&self.paths, reach.map(|r| &r.paths));
        let (except_names, except_globs, except) = pick(&self.except, reach.map(|r| &r.except));
        (
            Dir {
                path_names,
                path_globs,
                except_names,
                except_globs,
            },
            Reach { paths, except },
        )
    }

    /// [`Protected::step`] for an entry of a directory prepared with [`Protected::dir`]; the
    /// full path is built with `path` only when a glob needs it.
    pub fn step_in(
        &self,
        dir: &Dir,
        parent: State,
        name: &[u8],
        path: &dyn Fn() -> Vec<u8>,
    ) -> State {
        if parent == State::Sticky || self.names.matches(name) {
            return State::Sticky;
        }
        let fold = self.paths.fold;
        let key = |n: &[u8]| {
            if fold {
                n.to_ascii_lowercase()
            } else {
                n.to_vec()
            }
        };
        let hit = |names: &HashSet<Vec<u8>>, globs: &[usize], set: &Set| {
            (!names.is_empty() && names.contains(&key(name)))
                || (!globs.is_empty() && {
                    let p = path();
                    globs.iter().any(|&i| set.globs[i].matches(&p))
                })
        };
        let mut on = parent == State::On || hit(&dir.path_names, &dir.path_globs, &self.paths);
        if on && hit(&dir.except_names, &dir.except_globs, &self.except) {
            on = false;
        }
        if on { State::On } else { State::Off }
    }

    /// The state of the entry at `path` (`/`-separated, absolute), whose last component is
    /// `name`, in a directory whose state is `parent`.
    #[must_use]
    pub fn step(&self, parent: State, path: &[u8], name: &[u8]) -> State {
        if parent == State::Sticky || self.names.matches(name) {
            return State::Sticky;
        }
        let mut on = parent == State::On || self.paths.matches(path);
        if on && self.except.matches(path) {
            on = false;
        }
        if on { State::On } else { State::Off }
    }

    /// Whether the entry at `path` is protected, by its own name or path or by those of a
    /// directory above it: [`Protected::step`] down every prefix of the path.
    #[must_use]
    pub fn covers(&self, path: &[u8]) -> bool {
        self.state_of(path).protected()
    }

    /// The state of the entry at `path`, from the root down.
    #[must_use]
    pub fn state_of(&self, path: &[u8]) -> State {
        let mut state = State::Off;
        for prefix in prefixes(path) {
            let name = &prefix[prefix.iter().rposition(|&b| b == b'/').map_or(0, |i| i + 1)..];
            state = self.step(state, prefix, name);
        }
        state
    }
}

/// Every ancestor path of `path` and then the path itself (`/a`, `/a/b`, `/a/b/c`).
fn prefixes(path: &[u8]) -> impl Iterator<Item = &[u8]> {
    crate::matcher::ancestors(path).chain(std::iter::once(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mac() -> Protected {
        Protected::new(&Env::with(
            Platform::MacOs,
            &[
                ("HOME", "/Users/u"),
                ("TMPDIR", "/private/var/folders/ab/xyz/T"),
            ],
        ))
    }

    fn linux() -> Protected {
        Protected::new(&Env::with(Platform::Linux, &[("HOME", "/home/u")]))
    }

    #[test]
    fn names_protect_anywhere() {
        let p = linux();
        assert!(p.covers(b"/home/u/dev/proj/.git"));
        assert!(p.covers(b"/home/u/dev/proj/.git/objects/pack"));
        assert!(p.covers(b"/home/u/.ssh/id_ed25519"));
        assert!(p.covers(b"/home/u/vault.kdbx"));
        assert!(p.covers(b"/home/u/dev/app/.env.local"));
        assert!(!p.covers(b"/home/u/dev/proj/target"));
        assert!(!p.covers(b"/home/u/.cache/pip"));
    }

    #[test]
    fn system_paths_and_their_exceptions() {
        let p = mac();
        assert!(p.covers(b"/System/Library"));
        assert!(p.covers(b"/usr/bin/true"));
        assert!(!p.covers(b"/usr/local/Cellar/x"));
        assert!(p.covers(b"/dev/fd/3"));
        assert!(p.covers(b"/etc/hosts"));
        assert!(p.covers(b"/var/db/x"));
        assert!(p.covers(b"/private/var/folders/ab/xyz/C/com.apple.x"));
        assert!(!p.covers(b"/private/var/folders/ab/xyz/T/tmp.123"));
        assert!(p.covers(b"/Users/u/Library/Containers/com.x/Data/Documents"));
        assert!(!p.covers(b"/Users/u/Library/Containers/com.x/Data/Library/Caches/y"));
        assert!(p.covers(b"/Applications/X.app/Contents/Resources"));
        assert!(p.covers(b"/Users/u/Pictures/Photos Library.photoslibrary/originals"));
        // Other users' homes are protected, our own is not.
        assert!(p.covers(b"/Users/other/Library/Caches"));
        assert!(!p.covers(b"/Users/u/Library/Caches/pip"));
        assert!(!p.covers(b"/Users/Shared/x"));
        // An exception does not lift a protected name below it.
        assert!(p.covers(b"/Users/u/.ssh/config"));
        assert!(p.covers(b"/Users/u/.cargo/bin/cargo"));
        assert!(!p.covers(b"/Users/u/.cargo/registry/cache"));
    }

    #[test]
    fn linux_and_windows() {
        let p = linux();
        assert!(p.covers(b"/var/lib/docker/overlay2"));
        assert!(p.covers(b"/lib64/ld.so"));
        assert!(!p.covers(b"/var/cache/apt"));
        assert!(p.covers(b"/home/other/.cache"));
        assert!(!p.covers(b"/home/u/.cache"));
        assert!(p.covers(b"/home/u/.mozilla/firefox/abc.default/cookies.sqlite"));
        assert!(!p.covers(b"/home/u/.mozilla/firefox/abc.default/cache2/entries"));
        // The root itself is not a protected path, so nothing is protected by it.
        assert!(!p.covers(b"/tmp/x"));

        let w = Protected::new(&Env::with(
            Platform::Windows,
            &[
                ("USERPROFILE", r"C:\Users\u"),
                ("LOCALAPPDATA", r"C:\Users\u\AppData\Local"),
                ("APPDATA", r"C:\Users\u\AppData\Roaming"),
            ],
        ));
        assert!(w.covers(b"C:/Windows/System32/x.dll"));
        assert!(!w.covers(b"C:/Windows/Temp/x.tmp"));
        assert!(w.covers(b"C:/pagefile.sys"));
        assert!(w.covers(b"C:/Users/u/OneDrive - Work/doc.docx"));
        assert!(!w.covers(b"C:/Users/u/AppData/Local/Temp/x"));
        assert!(w.covers(b"C:/Users/other/AppData"));
    }

    /// Walking down a path one directory at a time with [`Protected::dir`] and
    /// [`Protected::step_in`] gives what [`Protected::covers`] gives for the whole path.
    #[test]
    fn stepping_per_directory_agrees_with_whole_paths() {
        let p = mac();
        for path in [
            "/usr/local/Cellar/x",
            "/usr/bin/true",
            "/Users/u/Library/Containers/com.x/Data/Library/Caches/y",
            "/Users/u/Library/Containers/com.x/Data/Documents/z",
            "/Users/other/Library/Caches",
            "/Users/u/.ssh/config",
            "/Users/u/dev/a/.git/objects",
            "/private/var/folders/ab/xyz/T/tmp.1",
            "/Applications/X.app/Contents",
            "/Users/u/Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw",
        ] {
            let mut state = State::Off;
            let mut dir = b"".to_vec();
            for part in path.split('/').skip(1) {
                let d = p.dir(&[&dir[..], b"/"].concat());
                let full = [&dir[..], b"/", part.as_bytes()].concat();
                state = p.step_in(&d, state, part.as_bytes(), &|| full.clone());
                dir = full;
            }
            assert_eq!(state.protected(), p.covers(path.as_bytes()), "{path}");
        }
    }
}

//! Categories and cleanup rules (format: `docs/rules.md`).
//!
//! A rule names a kind of disk use (a Rust `target/`, the npm cache), how safe it is to delete
//! and how. Rules are TOML: a built-in pack ships in the binary ([`builtin`]) and users add or
//! override rules in their rule directory ([`load()`]). [`RuleSet::classify`] decides, from one
//! directory listing, which rule an entry matches; conditions on the entry's own contents
//! (`child`, `marker`) are checked when its listing arrives ([`RuleSet::check_inside`]).
//! Protected paths ([`protected`]) are fixed in the code and win over every rule.

pub mod dirs;
mod glob;
pub mod hash;
pub mod linked;
pub mod load;
mod matcher;
pub mod protected;
pub mod schema;
pub mod vars;

pub use dfdu_tree::{RuleIx, Safety};
pub use glob::{Glob, GlobError};
pub use linked::{Linked, linked_outside};
pub use load::{LoadError, builtin, load, load_from, sources};
pub use matcher::{
    Candidate, Dir, Hit, Inside, MAX_RULES, Reach, ReadMarker, RuleSet, Stat, Verdict, stem,
};
pub use vars::{Env, ExpandError};

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Platform {
    MacOs,
    Linux,
    Windows,
}

impl Platform {
    #[must_use]
    pub fn current() -> Platform {
        if cfg!(target_os = "macos") {
            Platform::MacOs
        } else if cfg!(windows) {
            Platform::Windows
        } else {
            Platform::Linux
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Platform> {
        Some(match s {
            "macos" => Platform::MacOs,
            "linux" => Platform::Linux,
            "windows" => Platform::Windows,
            _ => return None,
        })
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Platform::MacOs => "macos",
            Platform::Linux => "linux",
            Platform::Windows => "windows",
        }
    }
}

/// How a match is cleaned up.
#[derive(Clone, Debug)]
pub enum Action {
    Delete,
    Trash,
    /// The owning tool's own commands (`rule.native`); loading checks that `command` is not
    /// empty.
    Native(Box<schema::NativeDef>),
    /// Reported with its size only.
    Report,
}

impl Action {
    /// Its name in rule files.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Action::Delete => "delete",
            Action::Trash => "trash",
            Action::Native(_) => "native",
            Action::Report => "report",
        }
    }

    /// The native commands of a `native` action.
    #[must_use]
    pub fn native(&self) -> Option<&schema::NativeDef> {
        match self {
            Action::Native(n) => Some(n),
            _ => None,
        }
    }
}

/// What kind of entry a rule matches.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Kind {
    Dir,
    File,
    Any,
}

/// The theme's colour slots for categories (`cat.*` tokens), in their order.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum CategoryColor {
    Cache,
    Build,
    Vm,
    Toolchain,
    Media,
    Download,
    Container,
    Other,
}

/// One category of the taxonomy.
#[derive(Debug)]
pub struct Category {
    pub id: &'static str,
    pub name: &'static str,
    pub color: CategoryColor,
}

const fn cat(id: &'static str, name: &'static str, color: CategoryColor) -> Category {
    Category { id, name, color }
}

/// Every category. A rule's `category` must be one of these ids. The index is the category's
/// number ([`category_index`]); 0 is never used, so a node can store 0 for "none".
pub const CATEGORIES: &[Category] = &[
    cat("", "", CategoryColor::Other),
    cat("dev.build", "Project build output", CategoryColor::Build),
    cat("dev.deps", "Project dependencies", CategoryColor::Build),
    cat(
        "dev.pkgcache",
        "Package manager caches",
        CategoryColor::Cache,
    ),
    cat(
        "dev.xcode",
        "Xcode and Apple simulators",
        CategoryColor::Toolchain,
    ),
    cat(
        "dev.android",
        "Android SDK and emulators",
        CategoryColor::Vm,
    ),
    cat(
        "dev.toolchains",
        "Toolchains and runtimes",
        CategoryColor::Toolchain,
    ),
    cat(
        "dev.containers",
        "Containers and VMs",
        CategoryColor::Container,
    ),
    cat("dev.ide", "IDE and editor caches", CategoryColor::Cache),
    cat("dev.nix", "Nix", CategoryColor::Toolchain),
    cat("sys.cache", "OS and user caches", CategoryColor::Cache),
    cat("sys.logs", "Logs", CategoryColor::Other),
    cat("sys.crash", "Crash reports and dumps", CategoryColor::Other),
    cat("sys.trash", "Trash", CategoryColor::Download),
    cat(
        "sys.updates",
        "OS update leftovers",
        CategoryColor::Download,
    ),
    cat("sys.snapshots", "Snapshots", CategoryColor::Other),
    cat("app.browser", "Browser caches", CategoryColor::Cache),
    cat("app.chat", "Chat and meeting apps", CategoryColor::Cache),
    cat("app.media", "Media apps", CategoryColor::Media),
    cat("app.games", "Games", CategoryColor::Media),
    cat("app.mail", "Mail", CategoryColor::Other),
    cat("app.mobile", "Mobile device data", CategoryColor::Vm),
    cat(
        "content.large-old",
        "Large, not recently used files",
        CategoryColor::Other,
    ),
    cat(
        "content.duplicates",
        "Duplicate files",
        CategoryColor::Other,
    ),
    cat(
        "content.installers",
        "Installers and disk images",
        CategoryColor::Download,
    ),
    cat("content.media", "Large video files", CategoryColor::Media),
    cat(
        "content.extracted",
        "Archives already extracted",
        CategoryColor::Download,
    ),
    cat(
        "content.empty",
        "Empty files and folders",
        CategoryColor::Other,
    ),
    cat(
        "content.broken-links",
        "Broken symbolic links",
        CategoryColor::Other,
    ),
];

/// The number of category `id`, `None` for an id outside the taxonomy.
#[must_use]
pub fn category_index(id: &str) -> Option<u16> {
    CATEGORIES
        .iter()
        .skip(1)
        .position(|c| c.id == id)
        .and_then(|i| u16::try_from(i + 1).ok())
}

/// A file inside a candidate whose presence (and first bytes) a rule checks.
#[derive(Clone, Debug)]
pub struct Marker {
    pub file: String,
    pub prefix: Option<Vec<u8>>,
    pub required: bool,
}

/// A runtime condition that moves a match to a greater safety level.
#[derive(Clone, Debug)]
pub struct Degrade {
    /// Holds when none of these exists next to the match.
    pub missing_sibling: Vec<String>,
    pub safety: Safety,
    pub reason: String,
}

/// Checks done before deleting, by the classifier or the deleter, not during the scan.
#[derive(Clone, Debug, Default)]
pub struct Guard {
    pub not_running: Vec<String>,
    pub not_vcs_tracked: bool,
}

/// Where a rule came from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Source {
    Builtin,
    File(std::path::PathBuf),
}

/// A checked, compiled rule.
#[derive(Clone, Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent flags of a rule file's schema, not states of one machine"
)]
pub struct Rule {
    pub id: String,
    /// Index into [`CATEGORIES`].
    pub category: u16,
    pub name: String,
    pub safety: Safety,
    pub action: Action,
    pub explanation: String,
    pub lost: String,
    pub min_size: u64,
    pub min_age_days: u32,
    pub priority: i32,
    pub requires_admin: bool,
    pub references: Vec<String>,
    pub kind: Kind,
    pub names: Vec<Glob>,
    pub paths: Vec<Glob>,
    pub exclude: Vec<Glob>,
    pub not_inside: Vec<Glob>,
    pub stop_descent: bool,
    pub top_most: bool,
    /// Each list needs one of its names next to the candidate (`{name_stem}` allowed).
    pub sibling: Vec<Vec<String>>,
    /// Each list needs one of its names inside the candidate.
    pub child: Vec<Vec<String>>,
    pub markers: Vec<Marker>,
    /// Files only: the file uses more disk than this (0: any size).
    pub larger_than: u64,
    /// Files only: the file was not modified for this many days (0: any age).
    pub not_modified_days: u32,
    /// Directories only: the directory has no entries.
    pub empty: bool,
    /// Files only: the entry is a symbolic link whose target does not exist.
    pub broken_link: bool,
    pub guard: Guard,
    pub degrade: Vec<Degrade>,
    pub source: Source,
}

impl Rule {
    #[must_use]
    pub fn category(&self) -> &'static Category {
        &CATEGORIES[self.category as usize]
    }

    /// Whether some condition can only be checked once the candidate's own listing is known.
    #[must_use]
    pub fn looks_inside(&self) -> bool {
        !self.child.is_empty() || self.markers.iter().any(|m| m.required) || self.empty
    }
}

/// A path as rules see it: `/`-separated, without Windows' `\\?\` prefix. The classifier and
/// the deletion-time protection check both read paths through this.
pub fn rule_path(path: &std::path::Path) -> Vec<u8> {
    let b = dfdu_tree::name_bytes(path.as_os_str()).into_owned();
    if cfg!(windows) {
        let b = b.strip_prefix(br"\\?\").map(<[u8]>::to_vec).unwrap_or(b);
        b.into_iter()
            .map(|c| if c == b'\\' { b'/' } else { c })
            .collect()
    } else {
        b
    }
}

/// Parses a size such as `100 MiB`, `1.5 GB` or `0`: binary units (KiB to TiB) and decimal
/// units (KB to TB), case as written.
#[must_use]
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the size is checked non-negative, and `as` saturates a size past u64::MAX"
)]
pub fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim();
    let split = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let n: f64 = num.parse().ok()?;
    let mult: f64 = match unit.trim() {
        "" | "B" => 1.0,
        "KiB" => 1024.0,
        "MiB" => 1024.0 * 1024.0,
        "GiB" => 1024.0 * 1024.0 * 1024.0,
        "TiB" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        "KB" => 1e3,
        "MB" => 1e6,
        "GB" => 1e9,
        "TB" => 1e12,
        _ => return None,
    };
    (n >= 0.0).then(|| (n * mult).round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(parse_size("100 MiB"), Some(100 << 20));
        assert_eq!(parse_size("1.5 GB"), Some(1_500_000_000));
        assert_eq!(parse_size("0"), Some(0));
        assert_eq!(parse_size("12MB"), Some(12_000_000));
        assert_eq!(parse_size("1 mb"), None);
        assert_eq!(parse_size("lots"), None);
    }

    #[test]
    fn categories() {
        assert_eq!(category_index("dev.build"), Some(1));
        assert_eq!(category_index(""), None);
        assert_eq!(category_index("dev.nope"), None);
        assert_eq!(
            CATEGORIES[category_index("sys.trash").unwrap() as usize].name,
            "Trash"
        );
        assert!(Safety::Safe < Safety::Review && Safety::Review < Safety::Protected);
    }
}

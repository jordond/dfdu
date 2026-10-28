//! Categories and cleanup rules (architecture §6, `categories-and-cleanup.md §9`).
//!
//! A rule names a kind of disk use (a Rust `target/`, the npm cache), how safe it is to delete
//! and how. Rules are TOML: a built-in pack ships in the binary ([`builtin`]) and users add or
//! override rules in their rule directory ([`load`]). [`RuleSet::classify`] decides, from one
//! directory listing, which rule an entry matches; conditions on the entry's own contents
//! (`child`, `marker`) are checked when its listing arrives ([`RuleSet::check_inside`]).
//! Protected paths ([`protected`]) are fixed in the code and win over every rule.

mod glob;
pub mod hash;
pub mod load;
mod matcher;
pub mod protected;
pub mod schema;
pub mod vars;

pub use glob::{Glob, GlobError};
pub use load::{LoadError, builtin, load};
pub use matcher::{Candidate, Dir, Hit, Inside, Reach, ReadMarker, RuleSet, Verdict};
pub use vars::Env;

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Platform {
    MacOs,
    Linux,
    Windows,
}

impl Platform {
    pub fn current() -> Platform {
        if cfg!(target_os = "macos") {
            Platform::MacOs
        } else if cfg!(windows) {
            Platform::Windows
        } else {
            Platform::Linux
        }
    }

    pub fn parse(s: &str) -> Option<Platform> {
        Some(match s {
            "macos" => Platform::MacOs,
            "linux" => Platform::Linux,
            "windows" => Platform::Windows,
            _ => return None,
        })
    }
}

/// What deleting loses (`categories-and-cleanup.md §2`). Ordered from least to most to lose,
/// so a runtime condition may only move a rule to a greater level.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum Safety {
    /// Pure cache, rebuilt transparently.
    Safe,
    /// Rebuilt on demand at a real cost (download, compile).
    Regenerable,
    /// May hold user work or state; shown with what is inside.
    Review,
    /// Reported only, or removed with the owner's own tool.
    Manual,
    /// Never offered for deletion.
    Protected,
}

impl Safety {
    /// Every level, in order: `ALL[s as usize] == s`.
    pub const ALL: [Safety; 5] = [
        Safety::Safe,
        Safety::Regenerable,
        Safety::Review,
        Safety::Manual,
        Safety::Protected,
    ];

    pub fn parse(s: &str) -> Option<Safety> {
        Some(match s {
            "safe" => Safety::Safe,
            "regenerable" => Safety::Regenerable,
            "review" => Safety::Review,
            "manual" => Safety::Manual,
            "protected" => Safety::Protected,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Safety::Safe => "safe",
            Safety::Regenerable => "regenerable",
            Safety::Review => "review",
            Safety::Manual => "manual",
            Safety::Protected => "protected",
        }
    }
}

/// How a match is cleaned up.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Action {
    Delete,
    Trash,
    /// The owning tool's own command (`rule.native`).
    Native,
    /// Reported with its size only.
    Report,
}

impl Action {
    pub fn parse(s: &str) -> Option<Action> {
        Some(match s {
            "delete" => Action::Delete,
            "trash" => Action::Trash,
            "native" => Action::Native,
            "report" => Action::Report,
            _ => return None,
        })
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
pub enum Colour {
    Cache,
    Build,
    Vm,
    Toolchain,
    Media,
    Download,
    Container,
    Other,
}

/// One category of the taxonomy (`categories-and-cleanup.md §3`).
#[derive(Debug)]
pub struct Category {
    pub id: &'static str,
    pub name: &'static str,
    pub colour: Colour,
}

const fn cat(id: &'static str, name: &'static str, colour: Colour) -> Category {
    Category { id, name, colour }
}

/// Every category. A rule's `category` must be one of these ids. The index is the category's
/// number ([`category_index`]); 0 is never used, so a node can store 0 for "none".
pub const CATEGORIES: &[Category] = &[
    cat("", "", Colour::Other),
    cat("dev.build", "Project build output", Colour::Build),
    cat("dev.deps", "Project dependencies", Colour::Build),
    cat("dev.pkgcache", "Package manager caches", Colour::Cache),
    cat("dev.xcode", "Xcode and Apple simulators", Colour::Toolchain),
    cat("dev.android", "Android SDK and emulators", Colour::Vm),
    cat(
        "dev.toolchains",
        "Toolchains and runtimes",
        Colour::Toolchain,
    ),
    cat("dev.containers", "Containers and VMs", Colour::Container),
    cat("dev.ide", "IDE and editor caches", Colour::Cache),
    cat("dev.nix", "Nix", Colour::Toolchain),
    cat("sys.cache", "OS and user caches", Colour::Cache),
    cat("sys.logs", "Logs", Colour::Other),
    cat("sys.crash", "Crash reports and dumps", Colour::Other),
    cat("sys.trash", "Trash", Colour::Download),
    cat("sys.updates", "OS update leftovers", Colour::Download),
    cat("sys.snapshots", "Snapshots", Colour::Other),
    cat("app.browser", "Browser caches", Colour::Cache),
    cat("app.chat", "Chat and meeting apps", Colour::Cache),
    cat("app.media", "Media apps", Colour::Media),
    cat("app.games", "Games", Colour::Media),
    cat("app.mail", "Mail", Colour::Other),
    cat("app.mobile", "Mobile device data", Colour::Vm),
    cat(
        "content.large-old",
        "Large, not recently used files",
        Colour::Other,
    ),
    cat("content.duplicates", "Duplicate files", Colour::Other),
    cat(
        "content.installers",
        "Installers and disk images",
        Colour::Download,
    ),
    cat("content.media", "Large video files", Colour::Media),
    cat(
        "content.extracted",
        "Archives already extracted",
        Colour::Download,
    ),
    cat("content.empty", "Empty files and folders", Colour::Other),
];

/// The number of category `id`, `None` for an id outside the taxonomy.
pub fn category_index(id: &str) -> Option<u16> {
    CATEGORIES
        .iter()
        .skip(1)
        .position(|c| c.id == id)
        .map(|i| i as u16 + 1)
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
    pub keep_newest: u32,
    pub group_by: String,
}

/// Where a rule came from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Source {
    Builtin,
    File(std::path::PathBuf),
}

/// A checked, compiled rule.
#[derive(Clone, Debug)]
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
    pub guard: Guard,
    pub degrade: Vec<Degrade>,
    pub native: Option<schema::NativeDef>,
    pub source: Source,
}

impl Rule {
    pub fn category(&self) -> &'static Category {
        &CATEGORIES[self.category as usize]
    }

    /// Whether some condition can only be checked once the candidate's own listing is known.
    pub fn looks_inside(&self) -> bool {
        !self.child.is_empty() || self.markers.iter().any(|m| m.required)
    }
}

/// Parses a size such as `100 MiB`, `1.5 GB` or `0`: binary units (KiB to TiB) and decimal
/// units (KB to TB), case as written.
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

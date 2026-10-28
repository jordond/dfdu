//! The rule file format as written (`docs/rules.md`). Unknown fields are
//! errors, so a misspelt key is reported instead of silently ignored.
//! [`crate::load`](mod@crate::load) checks the values and compiles a file into
//! [`crate::Rule`]s.

use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct File {
    pub schema_version: u32,
    #[serde(default)]
    pub rule: Vec<RuleDef>,
}

/// A value that may differ per platform: `x` or `{ macos = x, linux = x, windows = x }`.
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum PerOs<T> {
    All(T),
    Each {
        #[serde(default)]
        macos: Option<T>,
        #[serde(default)]
        linux: Option<T>,
        #[serde(default)]
        windows: Option<T>,
    },
}

impl<T: Default + Clone> Default for PerOs<T> {
    fn default() -> Self {
        PerOs::All(T::default())
    }
}

impl<T: Default + Clone> PerOs<T> {
    pub fn get(&self, p: crate::Platform) -> T {
        match self {
            PerOs::All(v) => v.clone(),
            PerOs::Each {
                macos,
                linux,
                windows,
            } => match p {
                crate::Platform::MacOs => macos,
                crate::Platform::Linux => linux,
                crate::Platform::Windows => windows,
            }
            .clone()
            .unwrap_or_default(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleDef {
    pub id: String,
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Required unless the entry only disables or overrides `enabled` of a built-in rule.
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub platforms: Option<Vec<String>>,
    /// Built-in rules only: the platforms whose paths were checked on a real machine. The
    /// rule is enabled by default only there (`docs/rules.md`).
    #[serde(default)]
    pub verified: Option<Vec<String>>,
    #[serde(default)]
    pub safety: Option<String>,
    #[serde(default)]
    pub action: Option<String>,
    #[serde(default)]
    pub explanation: Option<String>,
    #[serde(default)]
    pub lost: Option<String>,
    #[serde(default)]
    pub min_size: Option<String>,
    #[serde(default)]
    pub min_age_days: Option<u32>,
    #[serde(default)]
    pub priority: Option<i32>,
    #[serde(default)]
    pub requires_admin: Option<bool>,
    #[serde(default)]
    pub references: Vec<String>,
    #[serde(default, rename = "match")]
    pub matching: Option<MatchDef>,
    #[serde(default)]
    pub guard: GuardDef,
    #[serde(default)]
    pub degrade: DegradeDef,
    #[serde(default)]
    pub native: Option<NativeDef>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "mirrors the independent flags of a rule file's `[rule.match]` table"
)]
pub struct MatchDef {
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub names: PerOs<Vec<String>>,
    #[serde(default)]
    pub paths: PerOs<Vec<String>>,
    #[serde(default)]
    pub exclude: PerOs<Vec<String>>,
    #[serde(default)]
    pub stop_descent: bool,
    #[serde(default)]
    pub top_most: bool,
    #[serde(default)]
    pub sibling: Vec<AnyOf>,
    #[serde(default)]
    pub child: Vec<AnyOf>,
    #[serde(default)]
    pub marker: Vec<MarkerDef>,
    /// Files only: matches a file using more disk than this (`1 GiB`).
    #[serde(default)]
    pub larger_than: Option<String>,
    /// Files only: matches a file not modified for this many days.
    #[serde(default)]
    pub not_modified_days: Option<u32>,
    /// Directories only: matches a directory with nothing in it.
    #[serde(default)]
    pub empty: bool,
    /// Files only: matches a symbolic link whose target does not exist.
    #[serde(default)]
    pub broken_link: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnyOf {
    pub any_of: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarkerDef {
    pub file: String,
    #[serde(default)]
    pub prefix: Option<String>,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuardDef {
    #[serde(default)]
    pub not_running: PerOs<Vec<String>>,
    #[serde(default)]
    pub not_vcs_tracked: bool,
    #[serde(default)]
    pub not_inside: Vec<String>,
    #[serde(default)]
    pub keep_newest: u32,
    #[serde(default)]
    pub group_by: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DegradeDef {
    #[serde(default)]
    pub when: Vec<DegradeWhen>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DegradeWhen {
    #[serde(default)]
    pub missing_sibling: Vec<String>,
    pub safety: String,
    pub reason: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeDef {
    #[serde(default)]
    pub check: Vec<String>,
    #[serde(default)]
    pub pre: Vec<Vec<String>>,
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub size_command: Vec<String>,
    #[serde(default)]
    pub list_command: Vec<String>,
    #[serde(default)]
    pub dry_run: Vec<String>,
    /// Further-reaching forms of `command` (`docker system prune --all`), offered separately
    /// and each confirmed on its own.
    #[serde(default, rename = "variant")]
    pub variants: Vec<VariantDef>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VariantDef {
    /// What it adds, for the confirmation ("also unused images").
    pub name: String,
    pub command: Vec<String>,
    #[serde(default)]
    pub dry_run: Vec<String>,
}

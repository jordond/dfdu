//! Which matches of the rules are recommended for deletion: what the cleanup view marks first
//! and what `dfdu report` and `dfdu clean` call recommended.

use dfdu_rules::Rule;
use dfdu_tree::{Class, Safety};

/// Nothing changed this recently is recommended.
const QUIET_DAYS: i64 = 7;

/// Why a match is or is not recommended for deletion ([`verdict`]).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Recommended: `safe` and at least the rule's minimum size.
    Safe,
    /// Recommended: `regenerable`, at least the rule's minimum size, and unchanged for at least
    /// this many days.
    Stale(i64),
    /// Not recommended at this safety level (`review` or worse).
    Level(Safety),
    /// Not: in use by a running program.
    InUse,
    /// Not: tracked by version control.
    Tracked,
    /// Not: whether version control tracks it could not be told.
    VcsUnknown,
    /// Not: still being scanned.
    Scanning,
    /// Not: changed within this many days.
    Newer(i64),
    /// Not: below the rule's minimum size, in bytes.
    Below(u64),
}

impl Verdict {
    /// Whether the match is recommended.
    #[must_use]
    pub fn recommended(self) -> bool {
        matches!(self, Verdict::Safe | Verdict::Stale(_))
    }
}

/// Whether a match of `rule` is recommended for deletion, and why: `safe` above the rule's
/// minimum size, `regenerable` only when also unchanged for the rule's minimum age, never
/// `review` or worse, nothing changed in the last 7 days, in use, tracked by version control or
/// still being scanned. The safety level is decided first, then what is in use or tracked, the
/// scan, the age and the size. `size` is its disk usage, `age` the seconds since it last
/// changed (now less its mtime).
#[must_use]
pub fn verdict(rule: &Rule, class: Class, size: u64, age: i64, scanning: bool) -> Verdict {
    let days = |d: i64| age >= d * 86_400;
    let min_age = match class.safety {
        Safety::Safe => QUIET_DAYS,
        Safety::Regenerable => i64::from(rule.min_age_days).max(QUIET_DAYS),
        s => return Verdict::Level(s),
    };
    if class.has(Class::IN_USE) {
        Verdict::InUse
    } else if class.has(Class::VCS_TRACKED) {
        Verdict::Tracked
    } else if class.has(Class::VCS_UNKNOWN) {
        Verdict::VcsUnknown
    } else if scanning {
        Verdict::Scanning
    } else if !days(min_age) {
        Verdict::Newer(min_age)
    } else if size < rule.min_size {
        Verdict::Below(rule.min_size)
    } else if class.safety == Safety::Safe {
        Verdict::Safe
    } else {
        Verdict::Stale(min_age)
    }
}

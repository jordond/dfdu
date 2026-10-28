//! Which matches of the rules are recommended for deletion: what the cleanup view marks first
//! and what `dfdu report` and `dfdu clean` call recommended.

use dfdu_rules::Rule;
use dfdu_tree::{Class, Safety};

/// Nothing changed this recently is recommended.
const QUIET_DAYS: i64 = 7;

/// Whether a match of `rule` is recommended for deletion: `safe` above the rule's minimum
/// size, `regenerable` only when also unchanged for the rule's minimum age, never `review` or
/// worse, nothing changed in the last 7 days, in use, tracked by version control
/// or still being scanned. `size` is its disk usage, `mtime` and `now` in seconds since the
/// epoch.
#[must_use]
pub fn recommends(
    rule: &Rule,
    class: Class,
    size: u64,
    mtime: i64,
    scanning: bool,
    now: i64,
) -> bool {
    let days = |d: i64| now - mtime >= d * 86_400;
    let c = &class;
    let flagged = c.has(Class::IN_USE) || c.has(Class::VCS_TRACKED) || c.has(Class::VCS_UNKNOWN);
    if flagged || scanning || !days(QUIET_DAYS) {
        return false;
    }
    match c.safety {
        Safety::Safe => size >= rule.min_size,
        Safety::Regenerable => {
            size >= rule.min_size && days(i64::from(rule.min_age_days).max(QUIET_DAYS))
        }
        _ => false,
    }
}

//! What `dfdu report` prints: the table of the matches and their totals by safety level, or
//! each match and the summary in the JSON document (schema/report.v1.json).

use dfdu_rules::{Action, RuleSet};
use dfdu_tree::{Class, Safety};
use serde_json::{Map, Value, json};

use crate::matches::{Item, age_days, reclaimable};
use crate::output::plural;

/// How a match is cleaned up, in words for the table.
fn how(rules: &RuleSet, it: &Item) -> String {
    let r = rules.rule(it.class.rule);
    match &r.action {
        _ if it.safety() == Safety::Protected => "keep".into(),
        Action::Native(n) => format!(
            "run `{}`",
            dfdu_engine::shown_argv(&dfdu_engine::native_argv(&n.command, &it.path))
        ),
        Action::Report => "report only".into(),
        Action::Delete | Action::Trash => "delete".into(),
    }
}

/// Disk usage by safety level, and of the recommended items, protected ones left out of the
/// total.
pub(super) fn summary(items: &[Item]) -> Value {
    let mut by = Map::new();
    for s in Safety::ALL {
        let (n, b) = items
            .iter()
            .filter(|i| i.safety() == s)
            .fold((0u64, 0u64), |(n, b), i| (n + 1, b + i.disk));
        by.insert(s.as_str().into(), json!({ "items": n, "disk": b }));
    }
    let rec = items.iter().filter(|i| i.recommended);
    json!({
        "items": items.len(),
        "disk": reclaimable(items),
        "by_safety": by,
        "recommended": { "items": rec.clone().count(), "disk": rec.map(|i| i.disk).sum::<u64>() },
    })
}

/// Writes the table of `list` to `w`.
pub(super) fn table(
    w: &mut dyn std::io::Write,
    rules: &RuleSet,
    list: &[Item],
    now: i64,
    fail_above: Option<u64>,
    exceeded: bool,
) -> std::io::Result<()> {
    if list.is_empty() {
        return writeln!(w, "nothing found");
    }
    writeln!(
        w,
        "{:<12} {:>10} {:>6}  {:<28} PATH",
        "SAFETY", "SIZE", "AGE", "WHAT"
    )?;
    for it in list {
        let r = rules.rule(it.class.rule);
        let mut flags = vec![];
        if it.recommended {
            flags.push("recommended".to_string());
        }
        if it.class.has(Class::IN_USE) {
            flags.push("in use".into());
        }
        if it.class.has(Class::VCS_TRACKED) {
            flags.push("tracked by git".into());
        }
        if it.class.has(Class::VCS_UNKNOWN) {
            flags.push("git tracking unknown".into());
        }
        if let Some(d) = it.class.degraded {
            flags.push(r.degrade[d as usize].reason.clone());
        }
        if it.matches_inside > 0 {
            flags.push(format!(
                "without the {} inside",
                plural(it.matches_inside, "match", "matches")
            ));
        }
        flags.push(how(rules, it));
        writeln!(
            w,
            "{:<12} {:>10} {:>5}d  {:<28} {}  [{}]",
            it.safety().as_str(),
            dfdu_engine::human(it.disk),
            age_days(now, it.mtime),
            truncate(&r.name, 28),
            it.path.display(),
            flags.join("; ")
        )?;
    }
    writeln!(w)?;
    for s in Safety::ALL {
        let of: Vec<&Item> = list.iter().filter(|i| i.safety() == s).collect();
        if !of.is_empty() {
            writeln!(
                w,
                "{:<12} {:>10}  {}",
                s.as_str(),
                dfdu_engine::human(of.iter().map(|i| i.disk).sum()),
                plural(of.len(), "item", "items")
            )?;
        }
    }
    let rec: Vec<&Item> = list.iter().filter(|i| i.recommended).collect();
    writeln!(
        w,
        "{:<12} {:>10}  {} (dfdu clean deletes these)",
        "recommended",
        dfdu_engine::human(rec.iter().map(|i| i.disk).sum()),
        plural(rec.len(), "item", "items")
    )?;
    if let Some(t) = fail_above {
        writeln!(
            w,
            "{} {} (--fail-above {})",
            dfdu_engine::human(reclaimable(list)),
            if exceeded {
                "is over the limit"
            } else {
                "is within the limit"
            },
            dfdu_engine::human(t)
        )?;
    }
    Ok(())
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_owned()
    } else {
        let mut t: String = s.chars().take(n - 1).collect();
        t.push('…');
        t
    }
}

#[cfg(test)]
mod tests;

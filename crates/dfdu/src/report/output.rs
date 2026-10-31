//! What `dfdu report` prints: the table of the matches and their totals by safety level, or
//! each match and the summary in the JSON document (schema/report.v1.json).

use dfdu_rules::{Action, RuleSet};
use dfdu_tree::{Class, Kind, Safety};
use serde_json::{Map, Value, json};

use super::{Item, reclaimable};
use crate::json as js;

fn age_days(now: i64, mtime: i64) -> i64 {
    (now - mtime).max(0) / 86_400
}

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

/// One item in JSON (`schema/report.v1.json`, `$defs/item`).
pub fn item_json(rules: &RuleSet, it: &Item, now: i64) -> Value {
    let r = rules.rule(it.class.rule);
    let c = &it.class;
    let mut o = Map::new();
    js::put_host_path(&mut o, "path", &it.path);
    o.insert(
        "type".into(),
        if it.kind == Kind::Dir { "dir" } else { "file" }.into(),
    );
    o.insert("rule".into(), r.id.clone().into());
    o.insert("rule_name".into(), r.name.clone().into());
    o.insert("category".into(), r.category().id.into());
    o.insert("category_name".into(), r.category().name.into());
    o.insert("safety".into(), it.safety().as_str().into());
    o.insert("action".into(), r.action.as_str().into());
    if let Some(n) = r.action.native() {
        let argv = dfdu_engine::native_argv(&n.command, &it.path);
        o.insert("native_command".into(), argv.into());
    }
    o.insert("disk".into(), it.disk.into());
    o.insert("apparent".into(), it.apparent.into());
    o.insert("entries".into(), it.entries.into());
    o.insert("shared_outside".into(), it.shared_out.into());
    o.insert("mtime".into(), it.mtime.into());
    o.insert("age_days".into(), age_days(now, it.mtime).into());
    o.insert("recommended".into(), it.recommended.into());
    o.insert("checked".into(), c.has(Class::CHECKED).into());
    o.insert("in_use".into(), c.has(Class::IN_USE).into());
    o.insert("vcs_tracked".into(), c.has(Class::VCS_TRACKED).into());
    o.insert("scanning".into(), it.scanning.into());
    if it.matches_inside > 0 {
        o.insert("matches_inside".into(), it.matches_inside.into());
    }
    if let Some(d) = c.degraded {
        o.insert(
            "degraded".into(),
            r.degrade[d as usize].reason.clone().into(),
        );
    }
    o.insert("explanation".into(), r.explanation.clone().into());
    o.insert("lost".into(), r.lost.clone().into());
    Value::Object(o)
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
            let n = it.matches_inside;
            flags.push(format!(
                "without the {n} match{} inside",
                if n == 1 { "" } else { "es" }
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
                "{:<12} {:>10}  {} item{}",
                s.as_str(),
                dfdu_engine::human(of.iter().map(|i| i.disk).sum()),
                of.len(),
                if of.len() == 1 { "" } else { "s" }
            )?;
        }
    }
    let rec: Vec<&Item> = list.iter().filter(|i| i.recommended).collect();
    writeln!(
        w,
        "{:<12} {:>10}  {} item{} (dfdu clean deletes these)",
        "recommended",
        dfdu_engine::human(rec.iter().map(|i| i.disk).sum()),
        rec.len(),
        if rec.len() == 1 { "" } else { "s" }
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

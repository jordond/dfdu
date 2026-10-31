//! What `dfdu clean` prints: a line per item and a summary, or the JSON document
//! (schema/clean.v1.json).

use std::io::Write;

use dfdu_rules::{Action, RuleSet};
use dfdu_tree::HostPath;
use serde_json::{Value, json};

use super::{CleanArgs, ModeArg, Outcome};
use crate::json as js;
use crate::report::{self, Item};

fn verb(mode: ModeArg) -> &'static str {
    match mode {
        ModeArg::Trash => "moved to the trash",
        ModeArg::Stage | ModeArg::Delete => "deleted",
    }
}

/// Writes a line per item and the summary to `w`.
pub(super) fn print_table(
    w: &mut dyn Write,
    rules: &RuleSet,
    args: &CleanArgs,
    items: &[Item],
    outcomes: &[Outcome],
) -> std::io::Result<()> {
    if items.is_empty() {
        return writeln!(w, "nothing found");
    }
    for (it, o) in items.iter().zip(outcomes) {
        let what = match o {
            Outcome::Planned => "would delete".to_string(),
            Outcome::Deleted if matches!(rules.rule(it.class.rule).action, Action::Native(_)) => {
                "ran its command".into()
            }
            Outcome::Deleted => verb(args.mode).into(),
            Outcome::Skipped(_) => "skipped".into(),
            Outcome::Failed(_) => "FAILED".into(),
        };
        let why = o.reason().map(|r| format!("  ({r})")).unwrap_or_default();
        let how = match rules.rule(it.class.rule).action.native() {
            Some(n) => {
                let command = dfdu_engine::native_argv(&n.command, &it.path);
                format!("  `{}`", dfdu_engine::shown_argv(&command))
            }
            None => String::new(),
        };
        writeln!(
            w,
            "{:<14} {:>10}  {}{how}{why}",
            what,
            dfdu_engine::human(it.disk),
            it.path.display()
        )?;
    }
    writeln!(w)?;
    let rows: Vec<(u64, &Outcome)> = items.iter().map(|it| it.disk).zip(outcomes).collect();
    writeln!(w, "{}", summary(args.yes, args.mode, &rows))
}

/// The last line of the table, from each item's disk usage and outcome: what was done, or
/// for a dry run what would be.
fn summary(yes: bool, mode: ModeArg, rows: &[(u64, &Outcome)]) -> String {
    let sum = |f: &dyn Fn(&Outcome) -> bool| {
        rows.iter()
            .filter(|(_, o)| f(o))
            .fold((0usize, 0u64), |(n, b), (disk, _)| (n + 1, b + disk))
    };
    let plural = |n| if n == 1 { "" } else { "s" };
    if yes {
        let (n, b) = sum(&|o| *o == Outcome::Deleted);
        let (f, _) = sum(&|o| matches!(o, Outcome::Failed(_)));
        let failed = if f > 0 {
            format!(", {f} failed")
        } else {
            String::new()
        };
        format!(
            "{n} item{} ({}) {}{failed}",
            plural(n),
            dfdu_engine::human(b),
            verb(mode)
        )
    } else {
        let (n, b) = sum(&|o| *o == Outcome::Planned);
        format!(
            "dry run: {n} item{} ({}) would be {}; nothing was changed (--yes does it)",
            plural(n),
            dfdu_engine::human(b),
            verb(mode)
        )
    }
}

/// What `run` scanned and did, for its output.
pub(super) struct Results<'a> {
    pub(super) root: &'a std::path::Path,
    pub(super) items: &'a [Item],
    pub(super) outcomes: &'a [Outcome],
    pub(super) complete: bool,
    pub(super) now: i64,
}

pub(super) fn print_json(rules: &RuleSet, args: &CleanArgs, r: &Results) -> std::io::Result<()> {
    let &Results {
        root,
        items,
        outcomes,
        complete,
        now,
    } = r;
    let mut doc = js::document(&HostPath::local(root));
    doc.insert("generated_at".into(), now.into());
    doc.insert("complete".into(), complete.into());
    doc.insert("dry_run".into(), (!args.yes).into());
    let mode = match args.mode {
        ModeArg::Trash => "trash",
        ModeArg::Stage => "stage",
        ModeArg::Delete => "delete",
    };
    doc.insert("mode".into(), mode.into());
    let list: Vec<Value> = items
        .iter()
        .zip(outcomes)
        .map(|(it, o)| {
            let mut v = report::item_json(rules, it, now);
            v["outcome"] = o.name().into();
            if let Some(r) = o.reason() {
                v["reason"] = r.into();
            }
            v
        })
        .collect();
    doc.insert("items".into(), list.into());
    let count = |name: &str| {
        let (n, b) = items
            .iter()
            .zip(outcomes)
            .filter(|(_, o)| o.name() == name)
            .fold((0u64, 0u64), |(n, b), (it, _)| (n + 1, b + it.disk));
        json!({ "items": n, "disk": b })
    };
    doc.insert(
        "summary".into(),
        json!({
            "planned": count("planned"),
            "deleted": count("deleted"),
            "skipped": count("skipped"),
            "failed": count("failed"),
        }),
    );
    js::print(doc)
}

#[cfg(test)]
mod tests;

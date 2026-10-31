//! What `dfdu clean` prints: a line per item and a summary, or the JSON document
//! (schema/clean.v1.json).

use dfdu_engine::Model;
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

pub(super) fn print_table(
    rules: &RuleSet,
    args: &CleanArgs,
    items: &[Item],
    outcomes: &[Outcome],
) -> std::io::Result<()> {
    use std::io::Write;
    let mut w = std::io::BufWriter::new(std::io::stdout().lock());
    if items.is_empty() {
        writeln!(w, "nothing found")?;
        return w.flush();
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
    let sum = |f: &dyn Fn(&Outcome) -> bool| {
        items
            .iter()
            .zip(outcomes)
            .filter(|(_, o)| f(o))
            .fold((0usize, 0u64), |(n, b), (it, _)| (n + 1, b + it.disk))
    };
    writeln!(w)?;
    if args.yes {
        let (n, b) = sum(&|o| *o == Outcome::Deleted);
        let (f, _) = sum(&|o| matches!(o, Outcome::Failed(_)));
        writeln!(
            w,
            "{n} item{} ({}) {}{}",
            if n == 1 { "" } else { "s" },
            dfdu_engine::human(b),
            verb(args.mode),
            if f > 0 {
                format!(", {f} failed")
            } else {
                String::new()
            }
        )?;
    } else {
        let (n, b) = sum(&|o| *o == Outcome::Planned);
        writeln!(
            w,
            "dry run: {n} item{} ({}) would be {}; nothing was changed (--yes does it)",
            if n == 1 { "" } else { "s" },
            dfdu_engine::human(b),
            verb(args.mode)
        )?;
    }
    w.flush()
}

/// What `run` scanned and did, for its output.
pub(super) struct Results<'a> {
    pub(super) root: &'a std::path::Path,
    pub(super) items: &'a [Item],
    pub(super) outcomes: &'a [Outcome],
    pub(super) complete: bool,
    pub(super) now: i64,
}

pub(super) fn print_json(
    m: &Model,
    rules: &RuleSet,
    args: &CleanArgs,
    r: &Results,
) -> std::io::Result<()> {
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
            let mut v = report::item_json(m, rules, it, now);
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

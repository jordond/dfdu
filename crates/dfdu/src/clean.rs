//! `dfdu clean`: deletes the rules' matches that `dfdu report` recommends (or, with `--all`,
//! every match that passes the filters), after the same checks as the browser's deletes. It
//! is a dry run unless `--yes` is given, and runs the owning tools' commands only with
//! `--allow-commands`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::Args;
use dfdu_clean::Mode;
use dfdu_engine::{Command, Engine, Model};
use dfdu_rules::{Action, RuleSet, Safety};
use dfdu_scan::Backend;
use dfdu_tree::{Class, NodeRef};
use serde_json::{Map, Value, json};

use crate::report::{self, Filter, Item};
use crate::{EXIT_DELETE_FAILED, EXIT_PARTIAL, MountArgs, json as js};

#[derive(Args)]
pub struct CleanArgs {
    #[arg(default_value = ".")]
    pub path: PathBuf,
    #[command(flatten)]
    pub filter: Filter,
    /// Matches this safe or safer are considered: safe, regenerable or review. Manual and
    /// protected matches are never deleted.
    #[arg(long, value_name = "LEVEL", default_value = "regenerable", value_parser = clean_safety)]
    pub safety: Safety,
    /// Every match that passes the filters, not only the recommended ones (recommended: `safe`
    /// above the rule's minimum size, `regenerable` also unchanged for the rule's minimum age,
    /// nothing changed in the last 7 days).
    #[arg(long)]
    pub all: bool,
    /// trash (the default; frees no space until the trash is emptied), stage (moved aside at
    /// once, then deleted; what is left after a crash is offered by `dfdu recover`) or delete
    /// (permanently).
    #[arg(long, value_enum, default_value_t = ModeArg::Trash)]
    pub mode: ModeArg,
    /// Delete. Without it nothing is changed: the plan is printed.
    #[arg(long)]
    pub yes: bool,
    /// Run the owning tool's cleanup command for matches cleaned up that way (`docker system
    /// prune`, `go clean -cache`). Without it they are skipped.
    #[arg(long)]
    pub allow_commands: bool,
    /// Output: table or json (schema/clean.v1.json).
    #[arg(long, value_enum, default_value_t = report::ReportFormat::Table)]
    pub format: report::ReportFormat,
    /// Worker threads.
    #[arg(long)]
    pub threads: Option<usize>,
    #[command(flatten)]
    pub mounts: MountArgs,
}

#[derive(clap::ValueEnum, Clone, Copy, PartialEq, Debug)]
pub enum ModeArg {
    Trash,
    Stage,
    Delete,
}

impl ModeArg {
    fn mode(self) -> Mode {
        match self {
            ModeArg::Trash => Mode::Trash,
            ModeArg::Stage => Mode::Stage,
            ModeArg::Delete => Mode::Permanent,
        }
    }
}

fn clean_safety(s: &str) -> Result<Safety, String> {
    match report::safety_arg(s)? {
        Safety::Manual | Safety::Protected => {
            Err(format!("{s} matches are never deleted by dfdu clean"))
        }
        l => Ok(l),
    }
}

/// What happens to one item.
#[derive(Clone, Debug, PartialEq)]
enum Outcome {
    /// Would be deleted (dry run).
    Planned,
    /// Left alone, with the reason.
    Skipped(String),
    Deleted,
    /// The delete or command did not happen or did not finish, with the reason.
    Failed(String),
}

impl Outcome {
    fn name(&self) -> &'static str {
        match self {
            Outcome::Planned => "planned",
            Outcome::Skipped(_) => "skipped",
            Outcome::Deleted => "deleted",
            Outcome::Failed(_) => "failed",
        }
    }
    fn reason(&self) -> Option<&str> {
        match self {
            Outcome::Skipped(r) | Outcome::Failed(r) => Some(r),
            _ => None,
        }
    }
}

/// Why `it` is left alone, or `None` when it is deleted.
fn skip_reason(rules: &RuleSet, it: &Item, args: &CleanArgs) -> Option<String> {
    let r = rules.rule(it.class.rule);
    let c = &it.class;
    Some(if it.safety() > args.safety {
        format!(
            "{} (--safety {})",
            it.safety().as_str(),
            args.safety.as_str()
        )
    } else if c.has(Class::VCS_TRACKED) {
        "tracked by version control".into()
    } else if c.has(Class::IN_USE) {
        "in use: a process it belongs to is running".into()
    } else if !c.has(Class::CHECKED) && (r.guard.not_vcs_tracked || !r.guard.not_running.is_empty())
    {
        "its checks (git tracking, running processes) did not finish".into()
    } else if it.scanning {
        "still being scanned".into()
    } else if r.action == Action::Report {
        "reported only: the rule never deletes it".into()
    } else if r.action == Action::Native && !args.allow_commands {
        "cleaned up by its tool's command: --allow-commands runs it".into()
    } else if r.action == Action::Native && r.native.as_ref().is_none_or(|n| n.command.is_empty()) {
        "its rule has no command".into()
    } else if !args.all && !it.recommended {
        "not recommended (--all includes it)".into()
    } else {
        return None;
    })
}

pub fn run(args: CleanArgs, rules: Arc<RuleSet>) -> ExitCode {
    let (engine, _) = match report::scan(&args.path, args.mounts, rules.clone(), args.threads, true)
    {
        Ok(e) => e,
        Err(msg) => {
            eprintln!("dfdu: {msg}");
            return ExitCode::from(1);
        }
    };
    let now = report::now_secs();
    let (root, complete, items) = engine.read(|m| {
        let root = m.tree.roots().next().map(|r| m.tree.path_buf(r));
        let complete = m.tree.roots().next().is_some_and(|r| js::complete(m, r));
        (root, complete, report::items(m, &rules, &args.filter, now))
    });
    let Some(root) = root else {
        return ExitCode::from(1);
    };
    let mut outcomes: Vec<Outcome> = items
        .iter()
        .map(|it| match skip_reason(&rules, it, &args) {
            Some(r) => Outcome::Skipped(r),
            None => Outcome::Planned,
        })
        .collect();
    if args.yes {
        execute(&engine, &rules, &items, &mut outcomes, args.mode.mode());
    }
    let code = engine.read(|m| {
        let r = if args.format == report::ReportFormat::Json {
            print_json(m, &rules, &root, &args, &items, &outcomes, complete, now)
        } else {
            print_table(&rules, &args, &items, &outcomes)
        };
        if let Err(e) = r
            && e.kind() != std::io::ErrorKind::BrokenPipe
        {
            eprintln!("dfdu: writing the output: {e}");
            return ExitCode::from(1);
        }
        if outcomes.iter().any(|o| matches!(o, Outcome::Failed(_))) {
            ExitCode::from(EXIT_DELETE_FAILED)
        } else if !complete {
            ExitCode::from(EXIT_PARTIAL)
        } else {
            ExitCode::SUCCESS
        }
    });
    engine.shutdown(Duration::from_millis(500));
    code
}

/// Deletes the planned items in one plan, then runs the native commands one at a time.
fn execute<B: Backend>(
    engine: &Engine<B>,
    rules: &RuleSet,
    items: &[Item],
    outcomes: &mut [Outcome],
    mode: Mode,
) {
    let planned = |i: usize| outcomes[i] == Outcome::Planned;
    let native = |it: &Item| rules.rule(it.class.rule).action == Action::Native;
    let deletes: Vec<usize> = (0..items.len())
        .filter(|&i| planned(i) && !native(&items[i]))
        .collect();
    let commands: Vec<usize> = (0..items.len())
        .filter(|&i| planned(i) && native(&items[i]))
        .collect();
    if !deletes.is_empty() {
        let before = engine.read(|m| m.status.last_delete.as_ref().map(|d| d.plan));
        engine.send(Command::Delete {
            items: deletes.iter().map(|&i| items[i].node).collect(),
            mode,
        });
        let summary = loop {
            engine.wait_idle(None);
            let s = engine.read(|m| m.status.last_delete.clone());
            if s.as_ref().map(|d| d.plan) != before || engine.read(|m| m.status.fatal.is_some()) {
                break s;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let problems: HashMap<NodeRef, String> = summary
            .map(|s| s.problems.into_iter().collect())
            .unwrap_or_default();
        engine.read(|m| {
            for &i in &deletes {
                let node = items[i].node;
                outcomes[i] = match problems.get(&node) {
                    Some(why) => Outcome::Failed(why.clone()),
                    None if m.tree.resolve_visible(node).is_none() => Outcome::Deleted,
                    None => Outcome::Failed("not deleted".into()),
                };
            }
        });
        if mode == Mode::Stage {
            engine.send(Command::CommitStaged);
            while engine.read(|m| m.status.staged.0 > 0 && m.status.fatal.is_none()) {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
    for (n, &i) in commands.iter().enumerate() {
        let id = n as u64 + 1;
        engine.send(Command::RunNative {
            id,
            node: items[i].node,
            variant: None,
            dry_run: false,
        });
        let t0 = Instant::now();
        let done = loop {
            let d = engine.read(|m| {
                m.status
                    .native
                    .as_ref()
                    .filter(|r| r.id == id)
                    .and_then(|r| r.done.clone())
            });
            if let Some(d) = d {
                break d;
            }
            if engine.read(|m| m.status.fatal.is_some()) || t0.elapsed() > Duration::from_secs(3600)
            {
                break Err("did not finish".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        outcomes[i] = match done {
            Ok(()) => Outcome::Deleted,
            Err(e) => Outcome::Failed(e),
        };
    }
    engine.wait_idle(None);
}

fn verb(mode: ModeArg) -> &'static str {
    match mode {
        ModeArg::Trash => "moved to the trash",
        ModeArg::Stage | ModeArg::Delete => "deleted",
    }
}

fn print_table(
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
            Outcome::Deleted if rules.rule(it.class.rule).action == Action::Native => {
                "ran its command".into()
            }
            Outcome::Deleted => verb(args.mode).into(),
            Outcome::Skipped(_) => "skipped".into(),
            Outcome::Failed(_) => "FAILED".into(),
        };
        let why = o.reason().map(|r| format!("  ({r})")).unwrap_or_default();
        let how = match rules.rule(it.class.rule).native.as_ref() {
            Some(n) if rules.rule(it.class.rule).action == Action::Native => {
                format!("  `{}`", n.command.join(" "))
            }
            _ => String::new(),
        };
        writeln!(
            w,
            "{:<14} {:>10}  {}{how}{why}",
            what,
            dfdu_tui::human(it.disk),
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
    if !args.yes {
        let (n, b) = sum(&|o| *o == Outcome::Planned);
        writeln!(
            w,
            "dry run: {n} item{} ({}) would be {}; nothing was changed (--yes does it)",
            if n == 1 { "" } else { "s" },
            dfdu_tui::human(b),
            verb(args.mode)
        )?;
    } else {
        let (n, b) = sum(&|o| *o == Outcome::Deleted);
        let (f, _) = sum(&|o| matches!(o, Outcome::Failed(_)));
        writeln!(
            w,
            "{n} item{} ({}) {}{}",
            if n == 1 { "" } else { "s" },
            dfdu_tui::human(b),
            verb(args.mode),
            if f > 0 {
                format!(", {f} failed")
            } else {
                String::new()
            }
        )?;
    }
    w.flush()
}

#[allow(clippy::too_many_arguments)]
fn print_json(
    m: &Model,
    rules: &RuleSet,
    root: &std::path::Path,
    args: &CleanArgs,
    items: &[Item],
    outcomes: &[Outcome],
    complete: bool,
    now: i64,
) -> std::io::Result<()> {
    let mut doc = Map::new();
    doc.insert("schema_version".into(), js::SCHEMA_VERSION.into());
    doc.insert("tool".into(), js::tool());
    js::put_path(&mut doc, "root", root);
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
    let mut w = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut w, &Value::Object(doc))?;
    std::io::Write::write_all(&mut w, b"\n")
}

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
use dfdu_engine::{Command, Engine, Mode, Model, WaitIdle};
use dfdu_rules::{Action, RuleSet, Safety};
use dfdu_scan::Backend;
use dfdu_tree::{Class, HostPath, NodeRef};
use serde_json::{Value, json};

use crate::report::{self, Filter, Item};
use crate::{EXIT_DELETE_FAILED, EXIT_PARTIAL, MountArgs, json as js, launch, written};

#[derive(Args)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "command-line flags, each an independent switch"
)]
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
    /// First delete what earlier sessions left staged (`dfdu recover` lists it).
    #[arg(long, conflicts_with = "restore_staged")]
    pub finish_staged: bool,
    /// First put back what earlier sessions left staged.
    #[arg(long)]
    pub restore_staged: bool,
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
    } else if c.has(Class::VCS_UNKNOWN) {
        "git could not tell whether it tracks files in it".into()
    } else if c.has(Class::IN_USE) {
        "in use: a process it belongs to is running".into()
    } else if !c.has(Class::CHECKED) && (r.guard.not_vcs_tracked || !r.guard.not_running.is_empty())
    {
        "its checks (git tracking, running processes) did not finish".into()
    } else if it.scanning {
        "still being scanned".into()
    } else if matches!(r.action, Action::Report) {
        "reported only: the rule never deletes it".into()
    } else if matches!(r.action, Action::Native(_)) && !args.allow_commands {
        "cleaned up by its tool's command: --allow-commands runs it".into()
    } else if !args.all && !it.recommended {
        "not recommended (--all includes it)".into()
    } else {
        return None;
    })
}

/// Reports what earlier sessions left unfinished, and with `--finish-staged` or
/// `--restore-staged` (and `--yes`) finishes or restores their staged items. Returns whether
/// something failed.
fn earlier_sessions(args: &CleanArgs) -> bool {
    let Some(dir) = dfdu_clean::default_journal_dir() else {
        return false;
    };
    let mut r = match dfdu_clean::recover::scan(&dir) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("dfdu: {}: {e}", dir.display());
            return true;
        }
    };
    if r.items.is_empty() {
        return false;
    }
    let act = args.yes && (args.finish_staged || args.restore_staged);
    let mut failed = false;
    // Highest first, so the numbers of the others stay valid.
    for i in (0..r.items.len()).rev() {
        let staged = matches!(
            r.items[i].state,
            dfdu_clean::recover::Pending::Staged { .. }
                | dfdu_clean::recover::Pending::PartlyDeleted { .. }
        );
        if !act || !staged {
            continue;
        }
        let path = r.items[i].path.display().to_string();
        let res = if args.finish_staged {
            r.finish(i)
                .map(|()| format!("{path}: staged earlier, deleted"))
        } else {
            r.restore(i)
                .map(|to| format!("{path}: staged earlier, restored at {}", to.display()))
        };
        match res {
            Ok(m) => eprintln!("dfdu: {m}"),
            Err(e) => {
                eprintln!("dfdu: {path}: {e}");
                failed = true;
            }
        }
    }
    if !r.items.is_empty() {
        eprintln!(
            "dfdu: {} item{} earlier sessions left unfinished (dfdu recover lists them{})",
            r.items.len(),
            if r.items.len() == 1 { "" } else { "s" },
            if act {
                ""
            } else {
                "; --finish-staged or --restore-staged with --yes acts on staged ones"
            }
        );
    }
    failed
}

pub fn run(args: &CleanArgs, rules: &Arc<RuleSet>) -> ExitCode {
    let earlier_failed = earlier_sessions(args);
    let (engine, _) = match report::scan(&args.path, args.mounts, rules.clone(), args.threads, true)
    {
        Ok(e) => e,
        Err(code) => return code,
    };
    let now = dfdu_tree::now_secs();
    let (root, complete, items) = engine.read(|m| {
        let root = m.tree.roots().next().map(|r| m.tree.path_buf(r));
        let complete = m.tree.roots().next().is_some_and(|r| m.complete(r));
        (root, complete, report::items(m, rules, &args.filter, now))
    });
    let Some(root) = root else {
        return ExitCode::from(1);
    };
    let mut outcomes: Vec<Outcome> = items
        .iter()
        .map(|it| match skip_reason(rules, it, args) {
            Some(r) => Outcome::Skipped(r),
            None => Outcome::Planned,
        })
        .collect();
    if args.yes {
        execute(&engine, rules, &items, &mut outcomes, args.mode.mode());
    }
    let code = engine.read(|m| {
        let r = if args.format == report::ReportFormat::Json {
            print_json(
                m,
                rules,
                args,
                &Results {
                    root: &root,
                    items: &items,
                    outcomes: &outcomes,
                    complete,
                    now,
                },
            )
        } else {
            print_table(rules, args, &items, &outcomes)
        };
        if let Err(code) = written(r) {
            return code;
        }
        if earlier_failed || outcomes.iter().any(|o| matches!(o, Outcome::Failed(_))) {
            ExitCode::from(EXIT_DELETE_FAILED)
        } else if !complete {
            ExitCode::from(EXIT_PARTIAL)
        } else {
            ExitCode::SUCCESS
        }
    });
    engine.shutdown(launch::SHUTDOWN);
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
    let native = |it: &Item| matches!(rules.rule(it.class.rule).action, Action::Native(_));
    let deletes: Vec<usize> = (0..items.len())
        .filter(|&i| planned(i) && !native(&items[i]))
        .collect();
    let commands: Vec<usize> = (0..items.len())
        .filter(|&i| planned(i) && native(&items[i]))
        .collect();
    if !deletes.is_empty() {
        let before = engine.read(|m| m.status.last_delete.as_ref().map(|d| d.plan));
        engine.send(Command::Delete {
            items: deletes.iter().flat_map(|&i| items[i].targets()).collect(),
            mode,
        });
        let summary = loop {
            let stopped = matches!(engine.wait_idle(None), WaitIdle::Fatal(_));
            let s = engine.read(|m| m.status.last_delete.clone());
            if s.as_ref().map(|d| d.plan) != before || stopped {
                break s;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let problems: HashMap<NodeRef, String> = summary
            .map(|s| s.problems.into_iter().collect())
            .unwrap_or_default();
        engine.read(|m| {
            for &i in &deletes {
                let targets = items[i].targets();
                outcomes[i] = match targets.iter().find_map(|n| problems.get(n)) {
                    Some(why) => Outcome::Failed(why.clone()),
                    None if targets.iter().all(|&n| m.tree.resolve_visible(n).is_none()) => {
                        Outcome::Deleted
                    }
                    None => Outcome::Failed("not deleted".into()),
                };
            }
        });
        if mode == Mode::Stage {
            engine.send(Command::CommitStaged);
            while engine.read(|m| m.status.staged.items > 0 && m.status.fatal.is_none()) {
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
struct Results<'a> {
    root: &'a std::path::Path,
    items: &'a [Item],
    outcomes: &'a [Outcome],
    complete: bool,
    now: i64,
}

fn print_json(m: &Model, rules: &RuleSet, args: &CleanArgs, r: &Results) -> std::io::Result<()> {
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

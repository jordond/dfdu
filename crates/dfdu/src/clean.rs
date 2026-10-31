//! `dfdu clean`: deletes the rules' matches that `dfdu report` recommends (or, with `--all`,
//! every match that passes the filters). It is a dry run unless `--yes` is given, and runs the
//! owning tools' commands only with `--allow-commands`.
//!
//! The deletes go to the engine as the browser's do, with the same checks when they run. What
//! it picks is stricter than what the browser's cleanup view lets you tick, which holds back
//! only protected matches and shows the rest of these as reason chips: it also skips matches git
//! could not tell about, ones a running process uses, and ones whose git or process checks did not
//! finish, besides those still being scanned, report-only ones, ones only their tool's command
//! cleans (without `--allow-commands`) and, without `--all`, ones not recommended. What it
//! prints is in `output`.

mod output;

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::Args;
use dfdu_engine::{Command, Engine, Mode};
use dfdu_rules::{Action, RuleSet, Safety};
use dfdu_scan::Backend;
use dfdu_tree::{Class, NodeRef};

use crate::report::{self, Filter, Item};
use crate::{EXIT_DELETE_FAILED, EXIT_PARTIAL, MountArgs, launch, print_out, written};
use output::{Results, print_json, print_table};

/// How long one tool's cleanup command may run before it counts as not finished.
const NATIVE_WAIT: Duration = Duration::from_secs(3600);

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
    let Some(dir) = dfdu_sys::dirs::journal_dir() else {
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

pub fn run(args: &CleanArgs, rules: &Arc<RuleSet>) -> Result<ExitCode, ExitCode> {
    let earlier_failed = earlier_sessions(args);
    let (engine, _) = report::scan(&args.path, args.mounts, args.threads, rules.clone(), true)?;
    let now = dfdu_tree::now_secs();
    let (root, complete, items) = engine.read(|m| {
        let root = m.tree.roots().next().map(|r| m.tree.path_buf(r));
        let complete = m.tree.roots().next().is_some_and(|r| m.complete(r));
        (root, complete, report::items(m, rules, &args.filter, now))
    });
    let Some(root) = root else {
        return Err(ExitCode::from(1));
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
    engine.shutdown(launch::SHUTDOWN);
    if args.format == report::ReportFormat::Json {
        written(print_json(
            rules,
            args,
            &Results {
                root: &root,
                items: &items,
                outcomes: &outcomes,
                complete,
                now,
            },
        ))?;
    } else {
        print_out(|w| print_table(w, rules, args, &items, &outcomes))?;
    }
    Ok(
        if earlier_failed || outcomes.iter().any(|o| matches!(o, Outcome::Failed(_))) {
            ExitCode::from(EXIT_DELETE_FAILED)
        } else if !complete {
            ExitCode::from(EXIT_PARTIAL)
        } else {
            ExitCode::SUCCESS
        },
    )
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
        let summary = launch::poll(engine, None, |m| {
            (m.status.last_delete.as_ref())
                .filter(|d| Some(d.plan) != before)
                .cloned()
        });
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
            launch::poll(engine, None, |m| (m.status.staged.items == 0).then_some(()));
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
        let done = launch::poll(engine, Some(NATIVE_WAIT), |m| {
            (m.status.native.as_ref())
                .filter(|r| r.id == id)
                .and_then(|r| r.done.clone())
        })
        .unwrap_or_else(|| Err("did not finish".into()));
        outcomes[i] = match done {
            Ok(()) => Outcome::Deleted,
            Err(e) => Outcome::Failed(e),
        };
    }
    engine.wait_idle(None);
}

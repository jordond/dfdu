//! `dfdu report`: the rules' matches in a scan, largest first, with what each is, how safe it
//! is to delete and how. `dfdu clean` deletes a selection of the same list. What it prints is in
//! `output`.

mod output;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::Args;
use dfdu_engine::{Engine, Model};
use dfdu_rules::RuleSet;
use dfdu_scan::{Backend, NativeBackend};
use dfdu_tree::{Class, HostPath, Kind, NodeRef, Safety, now_secs};
use serde_json::json;

use crate::{EXIT_PARTIAL, MountArgs, json as js, launch, print_out, written};
pub use output::item_json;
use output::{summary, table};

/// Exit code when `--fail-above` is exceeded.
pub const EXIT_THRESHOLD: u8 = 4;
/// How long the guard checks (git tracking, running processes) may take after the scan.
const CHECK_WAIT: Duration = Duration::from_secs(60);

#[derive(Args, Clone)]
pub struct Filter {
    /// Only matches at least this big (`100M`, `1.5GiB`, `500 KB`).
    #[arg(long, value_name = "SIZE", value_parser = size_arg)]
    pub min_size: Option<u64>,
    /// Only these categories (comma separated): an id such as `dev.build`, or its first part
    /// (`dev`) for all of them.
    #[arg(long, value_name = "CATEGORY", value_delimiter = ',', value_parser = category_arg)]
    pub category: Vec<String>,
    /// Only matches unchanged for this long (`30d`, `2w`, `12h`; a bare number is days).
    #[arg(long, value_name = "AGE", value_parser = age_arg)]
    pub older_than: Option<u64>,
}

#[derive(Args)]
pub struct ReportArgs {
    #[arg(default_value = ".")]
    pub path: PathBuf,
    #[command(flatten)]
    pub filter: Filter,
    /// Only matches this safe or safer: safe, regenerable, review, manual or protected (all).
    #[arg(long, value_name = "LEVEL", value_parser = safety_arg)]
    pub safety: Option<Safety>,
    /// Exit with code 4 when the matches listed, protected ones left out, add up to more than
    /// this (`5GiB`), for CI.
    #[arg(long, value_name = "SIZE", value_parser = size_arg)]
    pub fail_above: Option<u64>,
    /// Output: table or json (schema/report.v1.json).
    #[arg(long, value_enum, default_value_t = ReportFormat::Table)]
    pub format: ReportFormat,
    /// Worker threads.
    #[arg(long)]
    pub threads: Option<usize>,
    #[command(flatten)]
    pub mounts: MountArgs,
}

#[derive(clap::ValueEnum, Clone, Copy, PartialEq)]
pub enum ReportFormat {
    Table,
    Json,
}

/// A size such as `100M`, `1.5GiB` or `500 KB`: `K`, `M`, `G` and `T` alone are binary units.
pub fn size_arg(s: &str) -> Result<u64, String> {
    let t = s.trim();
    let t = match t.strip_suffix(['K', 'M', 'G', 'T']) {
        Some(n) => format!("{n}{}iB", &t[n.len()..]),
        None => t.to_owned(),
    };
    dfdu_rules::parse_size(&t).ok_or_else(|| format!("`{s}` is not a size (100M, 1.5GiB, 500KB)"))
}

/// An age in seconds: `30d`, `2w`, `12h`, or a number of days.
pub fn age_arg(s: &str) -> Result<u64, String> {
    let t = s.trim();
    let (num, unit) = t.split_at(t.find(|c: char| !c.is_ascii_digit()).unwrap_or(t.len()));
    let mult = match unit {
        "" | "d" => 86_400,
        "w" => 7 * 86_400,
        "h" => 3_600,
        _ => 0,
    };
    match num.parse::<u64>() {
        Ok(n) if mult > 0 => n
            .checked_mul(mult)
            .ok_or_else(|| format!("`{s}` is too long")),
        _ => Err(format!("`{s}` is not an age (30d, 2w, 12h)")),
    }
}

pub fn safety_arg(s: &str) -> Result<Safety, String> {
    Safety::parse(s).ok_or_else(|| {
        format!("`{s}` is not a safety level (safe, regenerable, review, manual, protected)")
    })
}

fn category_arg(s: &str) -> Result<String, String> {
    let known = dfdu_rules::CATEGORIES.iter().skip(1);
    if known
        .clone()
        .any(|c| c.id == s || c.id.split('.').next() == Some(s))
    {
        Ok(s.to_owned())
    } else {
        let ids: Vec<&str> = known.map(|c| c.id).collect();
        Err(format!("`{s}` is not a category ({})", ids.join(", ")))
    }
}

/// One match of the rules, as the scan found it.
#[derive(Clone, Debug)]
pub struct Item {
    pub node: NodeRef,
    pub path: HostPath,
    pub kind: Kind,
    pub class: Class,
    pub disk: u64,
    pub apparent: u64,
    pub entries: u64,
    pub shared_out: u64,
    pub mtime: i64,
    pub scanning: bool,
    pub recommended: bool,
    /// How many matches inside it are listed on their own: its sizes leave them out.
    pub matches_inside: usize,
    /// With matches inside it, what cleaning it up deletes instead of the whole of it
    /// ([`dfdu_engine::Listed::rest`]).
    pub rest: Vec<NodeRef>,
}

impl Item {
    pub fn safety(&self) -> Safety {
        self.class.safety
    }

    /// What deleting it deletes: the node, or the rest of it when matches inside it are
    /// listed on their own.
    pub fn targets(&self) -> Vec<NodeRef> {
        if self.rest.is_empty() {
            vec![self.node]
        } else {
            self.rest.clone()
        }
    }
}

/// The listed matches ([`dfdu_engine::listed`]), largest first, that pass `f`: a match inside
/// one that does not stop the descent is listed on its own, and left out of that one.
pub fn items(m: &Model, rules: &RuleSet, f: &Filter, now: i64) -> Vec<Item> {
    dfdu_engine::listed(&m.tree, rules)
        .into_iter()
        .map(|l| item(m, rules, l, now))
        .filter(|it| keep(rules, f, it, now))
        .collect()
}

fn item(m: &Model, rules: &RuleSet, l: dfdu_engine::Listed, now: i64) -> Item {
    let t = &m.tree;
    let (ix, class, tot, scanning) = (l.ix, l.class, l.total, l.scanning);
    let mtime = t.mtime(ix);
    Item {
        node: t.node_ref(ix),
        path: l.path,
        kind: t.kind(ix),
        class,
        disk: tot.alloc,
        apparent: tot.apparent,
        entries: tot.items,
        shared_out: tot.shared_out,
        mtime,
        scanning,
        recommended: dfdu_engine::verdict(
            rules.rule(class.rule),
            class,
            tot.alloc,
            now.saturating_sub(mtime),
            scanning,
        )
        .recommended(),
        matches_inside: l.inner.len(),
        rest: l.rest.iter().map(|&r| t.node_ref(r)).collect(),
    }
}

fn keep(rules: &RuleSet, f: &Filter, it: &Item, now: i64) -> bool {
    let cat = rules.rule(it.class.rule).category().id;
    f.min_size.is_none_or(|min| it.disk >= min)
        && f.older_than.is_none_or(|age| {
            now.saturating_sub(it.mtime) >= i64::try_from(age).unwrap_or(i64::MAX)
        })
        && (f.category.is_empty()
            || f.category
                .iter()
                .any(|c| cat == c || cat.split('.').next() == Some(c.as_str())))
}

/// Starts an engine that scans `path` with the rules, and waits for the scan and the guard
/// checks of its matches, at most [`CHECK_WAIT`] for the checks: those not done by then are
/// said on standard error and left unchecked. How long that took. With `journal` the engine
/// can delete.
pub fn scan(
    path: &Path,
    mounts: MountArgs,
    threads: Option<usize>,
    rules: Arc<RuleSet>,
    journal: bool,
) -> Result<(Engine<NativeBackend>, Duration), ExitCode> {
    let t0 = Instant::now();
    let engine = launch::scan(path, mounts, threads, Some(rules), journal)?;
    if !wait_checks(&engine, CHECK_WAIT) {
        eprintln!("dfdu: {}", checks_cut_short(CHECK_WAIT));
    }
    Ok((engine, t0.elapsed()))
}

/// What [`scan`] says when the guard checks did not finish within `wait`.
fn checks_cut_short(wait: Duration) -> String {
    format!(
        "the git and process checks of some matches did not finish in {} s; they count as \
         not checked",
        wait.as_secs()
    )
}

/// Waits until every match whose rule has a guard was checked, or `timeout` passes; whether
/// they were.
fn wait_checks<B: Backend>(engine: &Engine<B>, timeout: Duration) -> bool {
    launch::wait_for(engine, Some(timeout), |m| guards_checked(m).then_some(())).is_some()
}

/// Whether every visible match whose rule has a guard was checked.
fn guards_checked(m: &Model) -> bool {
    let Some(rules) = &m.rules else { return true };
    m.tree.classes().all(|(ix, c)| {
        let g = &rules.rule(c.rule).guard;
        c.has(Class::CHECKED)
            || (!g.not_vcs_tracked && g.not_running.is_empty())
            || !m.tree.is_visible(ix)
    })
}

/// The disk usage of `items`, protected ones left out.
pub fn reclaimable(items: &[Item]) -> u64 {
    items
        .iter()
        .filter(|i| i.safety() != Safety::Protected)
        .map(|i| i.disk)
        .sum()
}

pub fn run(args: &ReportArgs, rules: &Arc<RuleSet>) -> Result<ExitCode, ExitCode> {
    let (engine, elapsed) = scan(&args.path, args.mounts, args.threads, rules.clone(), false)?;
    let now = now_secs();
    let code = engine.read(|m| {
        let Some(root) = m.tree.roots().next() else {
            return Err(ExitCode::from(1));
        };
        let max = args.safety.unwrap_or(Safety::Protected);
        let list: Vec<Item> = items(m, rules, &args.filter, now)
            .into_iter()
            .filter(|i| i.safety() <= max)
            .collect();
        let total = reclaimable(&list);
        let exceeded = args.fail_above.is_some_and(|t| total > t);
        let left_out = crate::outside::gather(m, root, true);
        if args.format == ReportFormat::Json {
            let mut doc = js::document(&m.tree.host_path(root));
            doc.insert("generated_at".into(), now.into());
            doc.insert("complete".into(), m.complete(root).into());
            doc.insert(
                "elapsed_ms".into(),
                u64::try_from(elapsed.as_millis())
                    .unwrap_or(u64::MAX)
                    .into(),
            );
            doc.insert(
                "items".into(),
                list.iter()
                    .map(|i| item_json(rules, i, now))
                    .collect::<Vec<_>>()
                    .into(),
            );
            doc.insert("summary".into(), summary(&list));
            crate::outside::put_json(&mut doc, &left_out, Some(rules));
            if let Some(t) = args.fail_above {
                doc.insert(
                    "threshold".into(),
                    json!({ "fail_above": t, "exceeded": exceeded }),
                );
            }
            written(js::print(doc))?;
        } else {
            let lines = crate::outside::lines(&left_out, Some(rules), "dfdu report");
            print_out(|w| {
                table(w, rules, &list, now, args.fail_above, exceeded)?;
                crate::outside::print(w, &lines)
            })?;
        }
        Ok(if exceeded {
            ExitCode::from(EXIT_THRESHOLD)
        } else if !m.complete(root) {
            ExitCode::from(EXIT_PARTIAL)
        } else {
            ExitCode::SUCCESS
        })
    });
    engine.shutdown(launch::SHUTDOWN);
    code
}

#[cfg(test)]
mod tests;

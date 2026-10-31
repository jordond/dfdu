//! `dfdu report`: the rules' matches in a scan, largest first, with what each is, how safe it
//! is to delete and how. `dfdu clean` deletes a selection of
//! the same list.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::Args;
use dfdu_engine::{Engine, Model};
use dfdu_rules::{Action, RuleSet, Safety};
use dfdu_scan::{Backend, NativeBackend};
use dfdu_tree::{Class, HostPath, Kind, NodeRef, now_secs};
use serde_json::{Map, Value, json};

use crate::{EXIT_PARTIAL, MountArgs, json as js, launch, written};

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
        recommended: dfdu_engine::recommends(
            rules.rule(class.rule),
            class,
            tot.alloc,
            now.saturating_sub(mtime),
            scanning,
        ),
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
/// checks of its matches; how long that took. With `journal` the engine can delete.
pub fn scan(
    path: &Path,
    mounts: MountArgs,
    rules: Arc<RuleSet>,
    threads: Option<usize>,
    journal: bool,
) -> Result<(Engine<NativeBackend>, Duration), ExitCode> {
    let t0 = Instant::now();
    let engine = launch::scan(path, mounts, threads, Some(rules), journal)?;
    wait_checks(&engine, CHECK_WAIT);
    Ok((engine, t0.elapsed()))
}

/// Waits until every match whose rule has a guard was checked, or `timeout` passes.
fn wait_checks<B: Backend>(engine: &Engine<B>, timeout: Duration) -> bool {
    let checked = |m: &Model| {
        let Some(rules) = &m.rules else { return true };
        m.tree.classes().all(|(ix, c)| {
            let g = &rules.rule(c.rule).guard;
            c.has(Class::CHECKED)
                || (!g.not_vcs_tracked && g.not_running.is_empty())
                || !m.tree.is_visible(ix)
        })
    };
    let t0 = Instant::now();
    while !engine.read(checked) {
        if t0.elapsed() > timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    true
}

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
pub fn item_json(m: &Model, rules: &RuleSet, it: &Item, now: i64) -> Value {
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
    let _ = m;
    Value::Object(o)
}

/// Disk usage by safety level, and of the recommended items, protected ones left out of the
/// total.
fn summary(items: &[Item]) -> Value {
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

/// The disk usage of `items`, protected ones left out.
pub fn reclaimable(items: &[Item]) -> u64 {
    items
        .iter()
        .filter(|i| i.safety() != Safety::Protected)
        .map(|i| i.disk)
        .sum()
}

pub fn run(args: &ReportArgs, rules: &Arc<RuleSet>) -> Result<ExitCode, ExitCode> {
    let (engine, elapsed) = scan(&args.path, args.mounts, rules.clone(), args.threads, false)?;
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
        let r = if args.format == ReportFormat::Json {
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
                    .map(|i| item_json(m, rules, i, now))
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
            js::print(doc)
        } else {
            let lines = crate::outside::lines(&left_out, Some(rules), "dfdu report");
            table(rules, &list, now, args.fail_above, exceeded)
                .and_then(|()| crate::outside::print(&mut std::io::stdout().lock(), &lines))
        };
        written(r)?;
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

fn table(
    rules: &RuleSet,
    list: &[Item],
    now: i64,
    fail_above: Option<u64>,
    exceeded: bool,
) -> std::io::Result<()> {
    use std::io::Write;
    let mut w = std::io::BufWriter::new(std::io::stdout().lock());
    if list.is_empty() {
        writeln!(w, "nothing found")?;
        return w.flush();
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
    w.flush()
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

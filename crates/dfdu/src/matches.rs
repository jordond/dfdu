//! The rules' matches in a scan, as `dfdu report` lists them and `dfdu clean` picks from them:
//! the scan that waits for their guard checks, the filters, each match with its sizes and
//! verdict, and its JSON.

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::Args;
use dfdu_engine::{Engine, Model};
use dfdu_rules::RuleSet;
use dfdu_scan::{Backend, NativeBackend};
use dfdu_tree::{Class, HostPath, Kind, NodeRef, Safety};
use serde_json::{Map, Value};

use crate::args::{age_arg, category_arg, size_arg};
use crate::output::put_host_path;
use crate::{MountArgs, launch};

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

/// The age of a match changed at `mtime`, in whole days; one from the future is new.
pub fn age_days(now: i64, mtime: i64) -> i64 {
    (now - mtime).max(0) / 86_400
}

/// One item in JSON (`schema/report.v1.json`, `$defs/item`).
pub fn item_json(rules: &RuleSet, it: &Item, now: i64) -> Value {
    let r = rules.rule(it.class.rule);
    let c = &it.class;
    let mut o = Map::new();
    put_host_path(&mut o, "path", &it.path);
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

#[cfg(test)]
pub(crate) mod tests;

//! Scan history in the CLI: `dfdu scan` records each scan, `dfdu history` lists them and
//! `dfdu diff` prints what grew (`safety-persistence-ops.md §3.4`).

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::Args;
use dfdu_engine::{Command, Engine, EngineOptions, Model};
use dfdu_scan::{JobOptions, native_backend};
use dfdu_store::history::{self, Change, History, Record, SizeMap, Sizes};
use dfdu_tree::Ix;
use serde_json::{Map, Value, json};

use crate::report::{ReportFormat, age_arg, now_secs, size_arg};
use crate::{EXIT_PARTIAL, MountArgs, json as js};

/// The key under which scans are compared ([`history::options_key`]).
pub fn options_key(job: &JobOptions, measured: bool) -> String {
    history::options_key(job.cross_mounts, job.network, measured)
}

/// Records the scan below `root` in the history database. Problems are reported, never fatal.
pub fn record(m: &Model, root: Ix, options: &str, started_at: i64) {
    let Some(path) = history::default_path() else {
        return;
    };
    let t = m.tree.total(root);
    let rows = history::rows(&m.tree, root);
    let r = History::open(&path).and_then(|mut h| {
        h.record(&Record {
            root: &m.tree.path_buf(root),
            options,
            started_at,
            finished_at: now_secs(),
            complete: js::complete(m, root),
            total: Sizes {
                disk: t.alloc,
                apparent: t.apparent,
                entries: t.items,
            },
            errors: t.errors,
            rows: &rows,
        })
    });
    if let Err(e) = r {
        eprintln!("dfdu: history {}: {e}", path.display());
    }
}

fn open() -> Result<History, ExitCode> {
    let Some(path) = history::default_path() else {
        eprintln!("dfdu: no state directory for the history");
        return Err(ExitCode::from(1));
    };
    History::open(&path).map_err(|e| {
        eprintln!("dfdu: history {}: {e}", path.display());
        ExitCode::from(1)
    })
}

/// `YYYY-MM-DD HH:MM UTC`.
pub fn date(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + (m <= 2) as i64;
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02} UTC",
        rem / 3_600,
        rem % 3_600 / 60
    )
}

#[derive(Args)]
pub struct HistoryArgs {
    /// Only scans of this directory.
    pub path: Option<PathBuf>,
}

/// `dfdu history`: the recorded scans, newest first.
pub fn list(args: HistoryArgs) -> ExitCode {
    let h = match open() {
        Ok(h) => h,
        Err(c) => return c,
    };
    let root = match args.path.as_deref().map(dunce::canonicalize).transpose() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("dfdu: {}: {e}", args.path.unwrap_or_default().display());
            return ExitCode::from(1);
        }
    };
    let scans = match h.scans(root.as_deref()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("dfdu: history: {e}");
            return ExitCode::from(1);
        }
    };
    use std::io::Write;
    let mut w = std::io::BufWriter::new(std::io::stdout().lock());
    let r = (|| {
        if scans.is_empty() {
            writeln!(w, "no scans recorded")?;
        }
        for s in scans {
            writeln!(
                w,
                "{}  {:>10}  {:>9} entries{}  {}",
                date(s.finished_at),
                dfdu_tui::human(s.total.disk),
                s.total.entries,
                if s.complete { "" } else { " (partial)" },
                s.root.display()
            )?;
        }
        w.flush()
    })();
    if let Err(e) = r
        && e.kind() != std::io::ErrorKind::BrokenPipe
    {
        eprintln!("dfdu: writing the output: {e}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

#[derive(Args)]
pub struct DiffArgs {
    /// A directory, scanned now and compared with its last recorded scan; or two files (a
    /// snapshot or an ncdu export each), the older first.
    #[arg(num_args = 0..=2, value_name = "PATH")]
    pub paths: Vec<PathBuf>,
    /// Compare with the recorded scan closest to this long ago (`7d`, `2w`, `12h`) instead of
    /// the last one.
    #[arg(long, value_name = "AGE", value_parser = age_arg)]
    pub since: Option<u64>,
    /// Levels below the root to list.
    #[arg(long, default_value_t = 3)]
    pub depth: usize,
    /// Changes to list, largest first.
    #[arg(long, default_value_t = 30)]
    pub top: usize,
    /// Leave out changes smaller than this (`1M`, `500K`).
    #[arg(long, value_name = "SIZE", value_parser = size_arg, default_value = "1M")]
    pub min_change: u64,
    /// Output: table or json (schema/diff.v1.json).
    #[arg(long, value_enum, default_value_t = ReportFormat::Table)]
    pub format: ReportFormat,
    /// Worker threads.
    #[arg(long)]
    pub threads: Option<usize>,
    #[command(flatten)]
    pub mounts: MountArgs,
}

/// One side of a diff: where it came from, when, and its directory sizes.
struct Side {
    source: String,
    at: Option<i64>,
    sizes: SizeMap,
}

fn side_json(s: &Side) -> Value {
    let mut o = Map::new();
    o.insert("source".into(), s.source.clone().into());
    if let Some(at) = s.at {
        o.insert("finished_at".into(), at.into());
    }
    Value::Object(o)
}

/// A file's tree as a side, every directory to `depth` levels (all sizes kept).
fn file_side(path: &Path, depth: usize) -> Result<(Side, PathBuf), ExitCode> {
    let im = crate::read_export(path)?;
    let root = im.tree.path_buf(im.root);
    let sizes = history::rows_to(&im.tree, im.root, depth, u64::MAX, usize::MAX);
    Ok((
        Side {
            source: path.display().to_string(),
            at: None,
            sizes,
        },
        root,
    ))
}

pub fn diff(args: DiffArgs) -> ExitCode {
    let (old, new, root, complete, partial) = if args.paths.len() == 2 {
        let (old, root) = match file_side(&args.paths[0], args.depth) {
            Ok(s) => s,
            Err(c) => return c,
        };
        let (new, _) = match file_side(&args.paths[1], args.depth) {
            Ok(s) => s,
            Err(c) => return c,
        };
        (old, new, root, usize::MAX, false)
    } else {
        match scan_and_base(&args) {
            Ok(v) => v,
            Err(c) => return c,
        }
    };
    let changes: Vec<Change> = history::diff(&old.sizes, &new.sizes, complete)
        .into_iter()
        .filter(|c| history::depth(&c.path) <= args.depth)
        .collect();
    let total = changes.iter().find(|c| c.path.is_empty()).cloned();
    let shown: Vec<&Change> = changes
        .iter()
        .filter(|c| !c.path.is_empty() && c.delta().unsigned_abs() >= args.min_change as u128)
        .take(args.top)
        .collect();
    let r = if args.format == ReportFormat::Json {
        print_json(&old, &new, &root, total.as_ref(), &shown)
    } else {
        print_table(&old, &new, &root, total.as_ref(), &shown)
    };
    if let Err(e) = r
        && e.kind() != std::io::ErrorKind::BrokenPipe
    {
        eprintln!("dfdu: writing the output: {e}");
        return ExitCode::from(1);
    }
    if partial {
        ExitCode::from(EXIT_PARTIAL)
    } else {
        ExitCode::SUCCESS
    }
}

/// Scans the directory now, finds the recorded scan to compare with, then records the new
/// one. Returns the old and new sides, the root, the depth to which both sides are complete,
/// and whether the new scan was partial.
fn scan_and_base(args: &DiffArgs) -> Result<(Side, Side, PathBuf, usize, bool), ExitCode> {
    let path = args
        .paths
        .first()
        .cloned()
        .unwrap_or_else(|| PathBuf::from("."));
    let h = open()?;
    let storage = dfdu_sys::storage(&path);
    let opts = EngineOptions {
        workers: args
            .threads
            .unwrap_or_else(|| dfdu_engine::workers_for(storage)),
        auto_abandon: Some(Duration::from_secs(30)),
        journal_dir: None,
        read_only: true,
        ..Default::default()
    };
    let measured = opts.measure_workers > 0;
    let started = now_secs();
    let engine = Engine::start(native_backend(storage), opts);
    let mut job = args.mounts.job();
    let key = options_key(&job, measured);
    // The matches of the rules are kept in the history at any depth.
    job.class = crate::RuleArgs::default()
        .load()
        .map(|r| dfdu_scan::classify::ClassOpts::for_root(r, &path));
    engine.send(Command::Scan { path, opts: job });
    engine.wait_idle(None);
    if let Some(msg) = engine.read(|m| m.status.fatal.clone()) {
        eprintln!("dfdu: {msg}");
        return Err(ExitCode::from(1));
    }
    let out = engine.read(|m| {
        let root_ix = m.tree.roots().next()?;
        let root = m.tree.path_buf(root_ix);
        let base = match args.since {
            Some(age) => h.nearest(&root, &key, started - age as i64),
            None => h.latest(&root, &key, None),
        };
        let partial = !js::complete(m, root_ix);
        let new = Side {
            source: "scan".into(),
            at: Some(now_secs()),
            sizes: history::rows(&m.tree, root_ix),
        };
        record(m, root_ix, &key, started);
        Some((root, base, new, partial))
    });
    engine.shutdown(Duration::from_millis(500));
    let Some((root, base, new, partial)) = out else {
        return Err(ExitCode::from(1));
    };
    let base = match base {
        Ok(Some(b)) => b,
        Ok(None) => {
            eprintln!(
                "dfdu: no earlier scan of {} with these options is recorded; this one is now",
                root.display()
            );
            return Err(ExitCode::from(1));
        }
        Err(e) => {
            eprintln!("dfdu: history: {e}");
            return Err(ExitCode::from(1));
        }
    };
    let sizes = h.sizes(base.id).map_err(|e| {
        eprintln!("dfdu: history: {e}");
        ExitCode::from(1)
    })?;
    let old = Side {
        source: "history".into(),
        at: Some(base.finished_at),
        sizes,
    };
    Ok((old, new, root, history::KEEP_DEPTH, partial))
}

fn signed(d: i128) -> String {
    let h = dfdu_tui::human(d.unsigned_abs().min(u64::MAX as u128) as u64);
    match d.signum() {
        1 => format!("+{h}"),
        -1 => format!("-{h}"),
        _ => h,
    }
}

fn shown_path(root: &Path, rel: &[u8]) -> String {
    if rel.is_empty() {
        root.display().to_string()
    } else {
        root.join(String::from_utf8_lossy(rel).as_ref())
            .display()
            .to_string()
    }
}

fn label(s: &Side) -> String {
    match s.at {
        Some(at) => format!("{} of {}", s.source, date(at)),
        None => s.source.clone(),
    }
}

fn print_table(
    old: &Side,
    new: &Side,
    root: &Path,
    total: Option<&Change>,
    shown: &[&Change],
) -> std::io::Result<()> {
    use std::io::Write;
    let mut w = std::io::BufWriter::new(std::io::stdout().lock());
    let size = |s: &Side| s.sizes.get(&b""[..]).map_or(0, |s| s.disk);
    writeln!(
        w,
        "{}: {} ({}) to {} ({}), {}",
        root.display(),
        dfdu_tui::human(size(old)),
        label(old),
        dfdu_tui::human(size(new)),
        label(new),
        signed(total.map_or(0, |c| c.delta()))
    )?;
    if shown.is_empty() {
        writeln!(w, "no other changes")?;
    }
    for c in shown {
        let what = match (c.old, c.new) {
            (None, _) => "  (new)",
            (_, None) => "  (gone)",
            _ => "",
        };
        writeln!(
            w,
            "{:>11}  {}{what}",
            signed(c.delta()),
            shown_path(root, &c.path)
        )?;
    }
    w.flush()
}

fn sizes_json(s: Option<Sizes>) -> Value {
    match s {
        Some(s) => json!({ "disk": s.disk, "apparent": s.apparent, "entries": s.entries }),
        None => Value::Null,
    }
}

fn change_json(root: &Path, c: &Change) -> Value {
    let mut o = Map::new();
    js::put_path(&mut o, "path", Path::new(&shown_path(root, &c.path)));
    o.insert(
        "status".into(),
        match (c.old, c.new) {
            (None, _) => "new",
            (_, None) => "gone",
            _ if c.delta() == 0 => "unchanged",
            _ if c.delta() > 0 => "grown",
            _ => "shrunk",
        }
        .into(),
    );
    o.insert("old".into(), sizes_json(c.old));
    o.insert("new".into(), sizes_json(c.new));
    o.insert("delta_disk".into(), (c.delta() as i64).into());
    o.insert("delta_apparent".into(), (c.apparent_delta() as i64).into());
    Value::Object(o)
}

fn print_json(
    old: &Side,
    new: &Side,
    root: &Path,
    total: Option<&Change>,
    shown: &[&Change],
) -> std::io::Result<()> {
    let mut doc = Map::new();
    doc.insert("schema_version".into(), js::SCHEMA_VERSION.into());
    doc.insert("tool".into(), js::tool());
    js::put_path(&mut doc, "root", root);
    doc.insert("old".into(), side_json(old));
    doc.insert("new".into(), side_json(new));
    let get = |s: &Side| s.sizes.get(&b""[..]).copied();
    let t = total.cloned().unwrap_or(Change {
        path: vec![],
        old: get(old),
        new: get(new),
    });
    doc.insert("total".into(), change_json(root, &t));
    doc.insert(
        "changes".into(),
        shown
            .iter()
            .map(|c| change_json(root, c))
            .collect::<Vec<_>>()
            .into(),
    );
    let mut w = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut w, &Value::Object(doc))?;
    std::io::Write::write_all(&mut w, b"\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_are_utc() {
        assert_eq!(date(0), "1970-01-01 00:00 UTC");
        assert_eq!(date(1_793_157_288), "2026-10-28 03:14 UTC");
        assert_eq!(date(951_782_400), "2000-02-29 00:00 UTC");
        assert_eq!(date(-86_400), "1969-12-31 00:00 UTC");
    }
}

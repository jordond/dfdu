//! `dfdu cache`: the scans the browser cached (`dfdu_store::cache`), listed or removed.

use std::process::ExitCode;

use clap::Subcommand;
use dfdu_store::cache::{Cache, Listed};
use dfdu_store::history::date_time;

use crate::written;

#[derive(Subcommand)]
pub enum CacheCmd {
    /// List the cached scans, newest first: when each was written, its size, its entries and
    /// its directory, and where the cache is.
    List,
    /// Remove every cached scan.
    Clear,
}

/// The cache, or `None` with the reason said.
fn open() -> Option<Cache> {
    if dfdu_store::cache::off_by_env() {
        eprintln!(
            "dfdu: the scan cache is off ({}=0)",
            dfdu_store::cache::OFF_VAR
        );
        return None;
    }
    let Some(dir) = dfdu_rules::dirs::cache_dir() else {
        eprintln!("dfdu: no cache directory");
        return None;
    };
    let cache = Cache::at(dir.clone());
    if cache.is_none() {
        eprintln!(
            "dfdu: {} belongs to another user; the scan cache is not used",
            dir.display()
        );
    }
    cache
}

pub fn run(cmd: &CacheCmd) -> ExitCode {
    let Some(cache) = open() else {
        return ExitCode::from(1);
    };
    match cmd {
        CacheCmd::List => list(&cache),
        CacheCmd::Clear => match cache.clear() {
            Ok(r) => {
                outln!(
                    "removed {} cached scan{} ({})",
                    r.files,
                    if r.files == 1 { "" } else { "s" },
                    dfdu_engine::human(r.bytes)
                );
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("dfdu: cache: {e}");
                ExitCode::from(1)
            }
        },
    }
}

/// One line of `dfdu cache list`.
fn line(l: &Listed) -> String {
    let when = date_time(i64::try_from(l.modified).unwrap_or(i64::MAX));
    let size = dfdu_engine::human(l.bytes);
    let entries = l.entries.map_or_else(|| "?".to_owned(), |n| n.to_string());
    match &l.scan {
        Some((root, h)) => {
            let full = date_time(i64::try_from(h.full_scan_at()).unwrap_or(i64::MAX));
            format!(
                "{when}  {size:>10}  {entries:>9} entries  {}  (full scan {full})",
                root.display()
            )
        }
        None => format!("{when}  {size:>10}  unreadable  {}", l.path.display()),
    }
}

fn list(cache: &Cache) -> ExitCode {
    use std::io::Write;
    let scans = cache.list();
    let mut w = std::io::BufWriter::new(std::io::stdout().lock());
    let r = (|| {
        if scans.is_empty() {
            writeln!(w, "no cached scans in {}", cache.dir().display())?;
        } else {
            for l in &scans {
                writeln!(w, "{}", line(l))?;
            }
            let total: u64 = scans.iter().map(|l| l.bytes).sum();
            writeln!(
                w,
                "{} in {}",
                dfdu_engine::human(total),
                cache.dir().display()
            )?;
        }
        w.flush()
    })();
    match written(r) {
        Ok(()) => ExitCode::SUCCESS,
        Err(code) => code,
    }
}

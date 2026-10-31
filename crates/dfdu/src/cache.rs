//! `dfdu cache`: the scans the browser cached (`dfdu_store::cache`), listed or removed.

use std::process::ExitCode;

use clap::Subcommand;
use dfdu_store::cache::{Cache, Listed};
use dfdu_store::history::date_time;

use crate::print_out;

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
    let Some(dir) = dfdu_sys::dirs::cache_dir() else {
        eprintln!("dfdu: no cache directory");
        return None;
    };
    match Cache::at(dir) {
        Ok(cache) => Some(cache),
        Err(e) => {
            eprintln!("dfdu: {e}; the scan cache is not used");
            None
        }
    }
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
    let scans = cache.list();
    let r = print_out(|w| {
        if scans.is_empty() {
            return writeln!(w, "no cached scans in {}", cache.dir().display());
        }
        for l in &scans {
            writeln!(w, "{}", line(l))?;
        }
        writeln!(
            w,
            "{} in {}",
            dfdu_engine::human(total(&scans)),
            cache.dir().display()
        )
    });
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(code) => code,
    }
}

/// The bytes the cached scans take.
fn total(scans: &[Listed]) -> u64 {
    scans.iter().map(|l| l.bytes).sum()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use dfdu_store::snapshot::Header;

    use super::*;

    fn listed(bytes: u64, scan: Option<(PathBuf, Header)>, entries: Option<u64>) -> Listed {
        Listed {
            path: PathBuf::from("/cache/0123456789abcdef.dfdu"),
            bytes,
            modified: 86_400 + 3_600 + 60,
            scan,
            entries,
        }
    }

    #[test]
    fn a_readable_scan_shows_its_root_entries_and_full_scan() {
        let h = Header {
            written: 2 * 86_400,
            last_full_scan: 86_400,
            ..Header::default()
        };
        let l = listed(2048, Some((PathBuf::from("/home/me"), h)), Some(1234));
        assert_eq!(
            line(&l),
            format!(
                "1970-01-02 01:01 UTC  {:>10}       1234 entries  /home/me  \
                 (full scan 1970-01-02 00:00 UTC)",
                dfdu_engine::human(2048)
            )
        );
    }

    #[test]
    fn a_header_without_a_full_scan_time_uses_when_it_was_written() {
        let h = Header {
            written: 2 * 86_400,
            ..Header::default()
        };
        let l = listed(1, Some((PathBuf::from("/r"), h)), None);
        let shown = line(&l);
        assert!(shown.contains("  ? entries"), "{shown}");
        assert!(
            shown.ends_with("(full scan 1970-01-03 00:00 UTC)"),
            "{shown}"
        );
    }

    #[test]
    fn an_unreadable_scan_shows_its_file() {
        let l = listed(10, None, None);
        let shown = line(&l);
        assert!(shown.starts_with("1970-01-02 01:01 UTC"), "{shown}");
        assert!(
            shown.ends_with("  unreadable  /cache/0123456789abcdef.dfdu"),
            "{shown}"
        );
    }

    #[test]
    fn a_time_past_i64_is_clamped_instead_of_wrapping() {
        let mut l = listed(0, None, None);
        l.modified = u64::MAX;
        assert!(line(&l).starts_with(&date_time(i64::MAX)));
    }

    #[test]
    fn the_total_adds_every_scan() {
        assert_eq!(total(&[]), 0);
        assert_eq!(total(&[listed(3, None, None), listed(4, None, None)]), 7);
    }
}

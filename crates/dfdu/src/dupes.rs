//! `dfdu dupes`: scans a directory, then compares the files that share a size
//! ([`dfdu_dupes`]) and lists the groups of identical files, the most space to free first.

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use clap::Args;
use dfdu_dupes::{Found, Options, Progress};
use serde_json::{Map, Value, json};

use crate::{EXIT_PARTIAL, MountArgs, json as js, launch, report, written};

#[derive(Args)]
pub struct DupesArgs {
    #[arg(default_value = ".")]
    path: PathBuf,
    /// Compare only files of at least this size (`1MiB`, `100KB`).
    #[arg(long, value_name = "SIZE", default_value = "1MiB", value_parser = report::size_arg)]
    min_size: u64,
    /// Output: table or json (schema/dupes.v1.json).
    #[arg(long, value_enum, default_value_t = Format::Table)]
    format: Format,
    /// Groups to print, the most space to free first [default: all].
    #[arg(long)]
    top: Option<usize>,
    /// Threads reading files [default: 4; 1 on spinning disks].
    #[arg(long)]
    threads: Option<usize>,
    #[command(flatten)]
    mounts: MountArgs,
}

#[derive(clap::ValueEnum, Clone, Copy, PartialEq)]
enum Format {
    Table,
    Json,
}

pub fn run(args: &DupesArgs) -> ExitCode {
    let t0 = Instant::now();
    // Its own --threads are the threads reading files.
    let engine = match launch::scan(&args.path, args.mounts, None, None, false) {
        Ok(e) => e,
        Err(code) => return code,
    };
    let (cands, errors) = engine.read(|m| {
        let Some(root) = m.tree.roots().next() else {
            return (Vec::new(), 0);
        };
        (
            dfdu_dupes::candidates(&m.tree, root, args.min_size),
            m.tree.total(root).errors,
        )
    });
    engine.shutdown(launch::SHUTDOWN);
    let defaults = Options::for_storage(dfdu_sys::storage(&args.path));
    let opts = Options {
        min_size: args.min_size,
        threads: args.threads.unwrap_or(defaults.threads),
    };
    let progress = Progress::default();
    let cancel = AtomicBool::new(false);
    let show = std::io::stderr().is_terminal();
    let found = std::thread::scope(|s| {
        let job = s.spawn(|| dfdu_dupes::find(&cands, &opts, &progress, &cancel));
        while !job.is_finished() {
            std::thread::sleep(Duration::from_millis(100));
            if show {
                eprint!("\r\x1b[K{}", progress_line(&progress));
            }
        }
        if show {
            eprint!("\r\x1b[K");
        }
        job.join().unwrap_or_default()
    });
    let w = &mut std::io::BufWriter::new(std::io::stdout().lock());
    let r = match args.format {
        Format::Table => {
            table(&found, args.top);
            Ok(())
        }
        Format::Json => {
            let doc = to_json(&found, args.top, args.min_size);
            serde_json::to_writer_pretty(&mut *w, &doc)
                .map_err(std::io::Error::from)
                .and_then(|()| std::io::Write::write_all(w, b"\n"))
        }
    };
    if let Err(code) = written(r.and_then(|()| std::io::Write::flush(w))) {
        return code;
    }
    if args.format == Format::Table {
        eprintln!(
            "{} files compared, {}{:.2}s",
            found.compared,
            match found.skipped {
                0 => String::new(),
                n => format!("{n} skipped (unreadable or changed), "),
            },
            t0.elapsed().as_secs_f64()
        );
    }
    if errors > 0 || found.skipped > 0 {
        ExitCode::from(EXIT_PARTIAL)
    } else {
        ExitCode::SUCCESS
    }
}

fn progress_line(p: &Progress) -> String {
    let get = |a: &std::sync::atomic::AtomicU64| a.load(Ordering::Relaxed);
    let round = match get(&p.round) {
        1 => "comparing starts and ends",
        _ => "comparing contents",
    };
    format!(
        "{round}: {}/{} files, {} of {}",
        get(&p.files),
        get(&p.round_files),
        dfdu_engine::human(get(&p.bytes)),
        dfdu_engine::human(get(&p.round_bytes)),
    )
}

fn table(found: &Found, top: Option<usize>) {
    let mut wasted = 0;
    for (i, g) in found.groups.iter().enumerate() {
        wasted += g.wasted();
        if top.is_some_and(|t| i >= t) {
            continue;
        }
        if i > 0 {
            outln!();
        }
        let free = if g.all_clones() {
            "clones sharing their data: deleting copies frees nothing".to_string()
        } else {
            format!("{} to free", dfdu_engine::human(g.wasted()))
        };
        outln!(
            "{} copies of {} ({free})",
            g.files.len(),
            dfdu_engine::human(g.size),
        );
        for f in &g.files {
            outln!("  {}", f.path.display());
        }
    }
    if found.groups.is_empty() {
        outln!("no duplicates");
    } else {
        outln!();
        outln!(
            "{} of identical files; deleting all but one of each would free {}",
            match found.groups.len() {
                1 => "1 group".to_string(),
                n => format!("{n} groups"),
            },
            dfdu_engine::human(wasted)
        );
    }
}

fn to_json(found: &Found, top: Option<usize>, min_size: u64) -> Value {
    let groups: Vec<Value> = found
        .groups
        .iter()
        .take(top.unwrap_or(usize::MAX))
        .map(|g| {
            let files: Vec<Value> = g
                .files
                .iter()
                .map(|f| {
                    let mut o = Map::new();
                    js::put_path(&mut o, "path", &f.path);
                    o.insert("disk".into(), f.alloc.into());
                    o.insert("mtime".into(), f.mtime.into());
                    o.insert("links".into(), f.links.into());
                    if let Some(p) = f.private {
                        o.insert("private".into(), p.into());
                    }
                    Value::Object(o)
                })
                .collect();
            json!({
                "size": g.size,
                "sha256": dfdu_dupes::hex(&g.hash),
                "wasted": g.wasted(),
                "clones": g.all_clones(),
                "files": files,
            })
        })
        .collect();
    json!({
        "schema_version": js::SCHEMA_VERSION,
        "tool": js::tool(),
        "min_size": min_size,
        "compared": found.compared,
        "skipped": found.skipped,
        "wasted": found.groups.iter().map(dfdu_dupes::Group::wasted).sum::<u64>(),
        "groups": groups,
    })
}

#[cfg(test)]
mod tests {
    use dfdu_dupes::{Candidate, Group};

    use super::*;

    fn file(path: &str, private: Option<u64>) -> Candidate {
        Candidate {
            path: path.into(),
            size: 100,
            alloc: 4096,
            dev_ino: (1, 0),
            mtime: 0,
            links: 0,
            private,
        }
    }

    fn group(files: Vec<Candidate>) -> Group {
        Group {
            size: 100,
            hash: [0xab; 32],
            files,
        }
    }

    #[test]
    fn json_lists_the_top_groups_and_the_waste_of_all() {
        let found = Found {
            groups: vec![
                group(vec![file("/a", None), file("/b", None)]),
                group(vec![file("/c", Some(0)), file("/d", Some(0))]),
                group(vec![file("/e", Some(1024)), file("/f", None)]),
            ],
            compared: 6,
            ..Found::default()
        };
        let doc = to_json(&found, Some(1), 1 << 20);
        assert_eq!(doc["groups"].as_array().map(Vec::len), Some(1));
        // Every group's waste, the ones left out by `top` included: the clones free nothing.
        assert_eq!(doc["wasted"], 2 * 4096);
        assert_eq!(doc["min_size"], 1 << 20);
        let g = &doc["groups"][0];
        assert_eq!(g["sha256"], "ab".repeat(32));
        assert_eq!(g["clones"], false);
        assert!(g["files"][0].get("private").is_none());
        let all = to_json(&found, None, 1);
        assert_eq!(all["groups"].as_array().map(Vec::len), Some(3));
        assert_eq!(all["groups"][1]["clones"], true);
        assert_eq!(all["groups"][1]["wasted"], 0);
        assert_eq!(all["groups"][2]["files"][0]["private"], 1024);
    }

    #[test]
    fn the_progress_line_names_its_round() {
        let p = Progress::default();
        p.round.store(1, Ordering::Relaxed);
        p.files.store(3, Ordering::Relaxed);
        p.round_files.store(10, Ordering::Relaxed);
        let line = progress_line(&p);
        assert!(
            line.starts_with("comparing starts and ends: 3/10 files, "),
            "{line}"
        );
        p.round.store(2, Ordering::Relaxed);
        assert!(progress_line(&p).starts_with("comparing contents: 3/10 files, "));
    }
}

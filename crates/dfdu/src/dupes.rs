//! `dfdu dupes`: scans a directory, then compares the files that share a size
//! ([`dfdu_dupes`]) and lists the groups of identical files, the most space to free first.

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use clap::Args;
use dfdu_dupes::{Found, Options, Progress};
use dfdu_engine::{Command, Engine, EngineOptions};
use dfdu_scan::native_backend;
use serde_json::{Map, Value, json};

use crate::{EXIT_PARTIAL, MountArgs, json as js, report};

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

pub fn run(args: DupesArgs) -> ExitCode {
    let storage = dfdu_sys::storage(&args.path);
    let spinning = dfdu_engine::workers_for(storage) <= 2;
    let engine = Engine::start(
        native_backend(storage),
        EngineOptions {
            workers: dfdu_engine::workers_for(storage),
            auto_abandon: Some(Duration::from_secs(30)),
            read_only: true,
            journal_dir: None,
            ..Default::default()
        },
    );
    let t0 = Instant::now();
    engine.send(Command::Scan {
        path: args.path.clone(),
        opts: args.mounts.job(),
    });
    engine.wait_idle(None);
    if let Some(msg) = engine.read(|m| m.status.fatal.clone()) {
        eprintln!("dfdu: {msg}");
        return ExitCode::from(1);
    }
    let (cands, errors) = engine.read(|m| {
        let Some(root) = m.tree.roots().next() else {
            return (Vec::new(), 0);
        };
        (
            dfdu_dupes::candidates(&m.tree, root, args.min_size),
            m.tree.total(root).errors,
        )
    });
    engine.shutdown(Duration::from_millis(200));
    let opts = Options {
        min_size: args.min_size,
        threads: args.threads.unwrap_or(if spinning { 1 } else { 4 }),
    };
    let progress = Progress::default();
    let cancel = AtomicBool::new(false);
    let show = std::io::stderr().is_terminal();
    let found = std::thread::scope(|s| {
        let job = s.spawn(|| dfdu_dupes::find(cands, &opts, &progress, &cancel));
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
    if let Err(e) = r.and_then(|()| std::io::Write::flush(w))
        && e.kind() != std::io::ErrorKind::BrokenPipe
    {
        eprintln!("dfdu: writing the output: {e}");
        return ExitCode::from(1);
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
        dfdu_tui::human(get(&p.bytes)),
        dfdu_tui::human(get(&p.round_bytes)),
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
            format!("{} to free", dfdu_tui::human(g.wasted()))
        };
        outln!(
            "{} copies of {} ({free})",
            g.files.len(),
            dfdu_tui::human(g.size),
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
            dfdu_tui::human(wasted)
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
        "wasted": found.groups.iter().map(|g| g.wasted()).sum::<u64>(),
        "groups": groups,
    })
}

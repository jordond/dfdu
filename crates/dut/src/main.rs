use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::{Args, Parser, Subcommand};
use dut_engine::{Command, Engine, EngineOptions, Model};
use dut_scan::{JobOptions, PortableBackend};
use dut_tree::{Ix, Tree};

#[derive(Parser)]
#[command(version, about, args_conflicts_with_subcommands = true)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
    /// Directory to browse (opens the interactive browser when stdout is a terminal).
    path: Option<PathBuf>,
    /// Descend into other filesystems.
    #[arg(long)]
    cross_mounts: bool,
    /// Never delete anything (the delete keys are refused).
    #[arg(long)]
    read_only: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// Scan a directory and print a summary.
    Scan(ScanArgs),
}

#[derive(Args)]
struct ScanArgs {
    #[arg(default_value = ".")]
    path: PathBuf,
    /// Levels of directories to print below the root.
    #[arg(long, default_value_t = 1)]
    depth: usize,
    /// Entries to print per directory, largest first.
    #[arg(long, default_value_t = 10)]
    top: usize,
    /// Show apparent sizes instead of disk usage.
    #[arg(long)]
    apparent: bool,
    /// Worker threads.
    #[arg(long)]
    threads: Option<usize>,
    /// Descend into other filesystems.
    #[arg(long)]
    cross_mounts: bool,
    /// Print only the root total in bytes (for scripts and tests).
    #[arg(long)]
    bytes: bool,
    /// Abandon directories stalled this many seconds.
    #[arg(long, default_value_t = 30)]
    stall_abandon: u64,
}

/// Exit codes from `docs/research/safety-persistence-ops.md` §4.3.
const EXIT_PARTIAL: u8 = 3;

/// Writes a crash report for a panic on any thread, then runs the previous hook (which, in
/// the browser, restores the terminal first).
fn install_crash_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(dir) = dut_engine::default_crash_dir() {
            let thread = std::thread::current();
            let report = format!(
                "dut {} crash report\n\nthread: {}\npanic: {info}\n\nbacktrace:\n{}\n",
                env!("CARGO_PKG_VERSION"),
                thread.name().unwrap_or("?"),
                std::backtrace::Backtrace::force_capture()
            );
            if let Some(p) = dut_engine::write_crash_report(&dir, &report) {
                eprintln!("dut: crash report written to {}", p.display());
            }
        }
        previous(info);
    }));
}

fn main() -> ExitCode {
    install_crash_hook();
    let cli = Cli::parse();
    match cli.cmd {
        Some(Cmd::Scan(args)) => scan(args),
        None => {
            let path = cli.path.unwrap_or_else(|| PathBuf::from("."));
            if !std::io::stdout().is_terminal() {
                return scan(ScanArgs {
                    path,
                    depth: 1,
                    top: 10,
                    apparent: false,
                    threads: None,
                    cross_mounts: cli.cross_mounts,
                    bytes: false,
                    stall_abandon: 30,
                });
            }
            let opts = dut_tui::RunOptions {
                path,
                cross_mounts: cli.cross_mounts,
                engine: EngineOptions {
                    read_only: cli.read_only,
                    ..Default::default()
                },
            };
            match dut_tui::run(opts) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("dut: {e}");
                    ExitCode::from(1)
                }
            }
        }
    }
}

fn scan(args: ScanArgs) -> ExitCode {
    let ScanArgs {
        path,
        depth,
        top,
        apparent,
        threads,
        cross_mounts,
        bytes,
        stall_abandon,
    } = args;
    let mut opts = EngineOptions {
        auto_abandon: Some(Duration::from_secs(stall_abandon)),
        ..Default::default()
    };
    if let Some(t) = threads {
        opts.workers = t;
    }
    let engine = Engine::start(PortableBackend, opts);
    let t0 = Instant::now();
    engine.send(Command::Scan {
        path,
        opts: JobOptions { cross_mounts },
    });
    engine.wait_idle(None);
    if let Some(msg) = engine.read(|m| m.status.fatal.clone()) {
        eprintln!("dut: {msg}");
        return ExitCode::from(1);
    }
    let elapsed = t0.elapsed();
    let code = engine.read(|m| {
        let Some(root) = m.tree.roots().next() else {
            return ExitCode::from(1);
        };
        let errors = m.tree.total(root).errors;
        if bytes {
            let t = m.tree.total(root);
            println!("{}", if apparent { t.apparent } else { t.alloc });
        } else {
            print_tree(&m.tree, root, depth, top, apparent);
            print_footer(m, root, elapsed);
        }
        if errors > 0 || m.status.incomplete {
            ExitCode::from(EXIT_PARTIAL)
        } else {
            ExitCode::SUCCESS
        }
    });
    engine.shutdown(Duration::from_millis(500));
    code
}

fn human(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

fn size(tree: &Tree, ix: Ix, apparent: bool) -> u64 {
    let t = tree.total(ix);
    if apparent { t.apparent } else { t.alloc }
}

fn print_tree(tree: &Tree, root: Ix, depth: usize, top: usize, apparent: bool) {
    println!(
        "{:>10}  {}",
        human(size(tree, root, apparent)),
        String::from_utf8_lossy(tree.name(root))
    );
    let mut stack = vec![(root, 0usize)];
    while let Some((ix, d)) = stack.pop() {
        if d >= depth {
            continue;
        }
        let mut kids: Vec<Ix> = tree.children(ix).collect();
        kids.sort_by_key(|&c| std::cmp::Reverse(size(tree, c, apparent)));
        let shown: Vec<Ix> = kids.into_iter().take(top).collect();
        for &c in shown.iter().rev() {
            stack.push((c, d + 1));
        }
        for &c in &shown {
            let slash = if tree.dir(c).is_some() { "/" } else { "" };
            println!(
                "{:>10}  {}{}{}",
                human(size(tree, c, apparent)),
                "  ".repeat(d + 1),
                String::from_utf8_lossy(tree.name(c)),
                slash
            );
            let _ = c;
        }
        break;
    }
}

fn print_footer(m: &Model, root: Ix, elapsed: Duration) {
    let t = m.tree.total(root);
    let s = &m.status.stats;
    eprintln!(
        "{} items, {} errors, {:.2}s{}",
        t.items,
        t.errors,
        elapsed.as_secs_f64(),
        if m.status.incomplete {
            ", incomplete"
        } else {
            ""
        }
    );
    if s.corrupt + s.lost + s.duplicate > 0 {
        eprintln!(
            "pipeline: corrupt {} lost {} duplicate {}",
            s.corrupt, s.lost, s.duplicate
        );
    }
}

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::{Args, Parser, Subcommand};
use dut_engine::{Command, Engine, EngineOptions, Model};
use dut_scan::{JobOptions, PortableBackend};
use dut_tree::{DirState, Ix, NodeFlags, Tree};
use dut_tui::SizeMode;

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
    #[arg(long, conflicts_with = "like_du")]
    apparent: bool,
    /// Count data shared between files (btrfs snapshots, reflinks) once per file, as du does,
    /// instead of once.
    #[arg(long)]
    like_du: bool,
    /// Worker threads.
    #[arg(long)]
    threads: Option<usize>,
    /// Threads measuring shared extents (btrfs, XFS); 0 reports sizes as du does.
    #[arg(long)]
    measure_workers: Option<usize>,
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
                    like_du: false,
                    threads: None,
                    measure_workers: None,
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
        like_du,
        threads,
        measure_workers,
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
    if let Some(n) = measure_workers {
        opts.measure_workers = n;
    }
    let engine = Engine::start(PortableBackend, opts);
    let t0 = Instant::now();
    engine.send(Command::Scan {
        path,
        opts: JobOptions {
            cross_mounts,
            ..Default::default()
        },
    });
    engine.wait_idle(None);
    if let Some(msg) = engine.read(|m| m.status.fatal.clone()) {
        eprintln!("dut: {msg}");
        return ExitCode::from(1);
    }
    let elapsed = t0.elapsed();
    let mode = if apparent {
        SizeMode::Apparent
    } else if like_du {
        SizeMode::Referenced
    } else {
        SizeMode::Disk
    };
    let code = engine.read(|m| {
        let Some(root) = m.tree.roots().next() else {
            return ExitCode::from(1);
        };
        let errors = m.tree.total(root).errors;
        if bytes {
            let t = m.tree.total(root);
            println!("{}", mode.of(&t));
        } else {
            print_tree(m, root, depth, top, mode);
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

fn size(tree: &Tree, ix: Ix, mode: SizeMode) -> u64 {
    mode.of(&tree.total(ix))
}

fn print_tree(m: &Model, root: Ix, depth: usize, top: usize, mode: SizeMode) {
    let tree = &m.tree;
    println!(
        "{:>10}  {}{}",
        dut_tui::human(size(tree, root, mode)),
        String::from_utf8_lossy(tree.name(root)),
        note(m, root)
    );
    // Depth first, largest first, `top` entries per directory.
    let mut stack = vec![(root, 0usize)];
    while let Some((ix, d)) = stack.pop() {
        if ix != root {
            let slash = if tree.dir(ix).is_some() { "/" } else { "" };
            println!(
                "{:>10}  {}{}{}{}",
                dut_tui::human(size(tree, ix, mode)),
                "  ".repeat(d),
                String::from_utf8_lossy(tree.name(ix)),
                slash,
                note(m, ix)
            );
        }
        if d >= depth {
            continue;
        }
        // Data the entries share is counted at the directory (`docs/btrfs.md` §3.4).
        if mode == SizeMode::Disk && tree.home(ix) > 0 {
            println!(
                "{:>10}  {}(shared between entries)",
                dut_tui::human(tree.home(ix)),
                "  ".repeat(d + 1)
            );
        }
        let mut kids: Vec<Ix> = tree.children(ix).collect();
        kids.sort_by_key(|&c| std::cmp::Reverse(size(tree, c, mode)));
        for &c in kids.iter().take(top).rev() {
            stack.push((c, d + 1));
        }
    }
}

/// Why an entry looks the way it does: not scanned, a second mount, a btrfs subvolume.
fn note(m: &Model, ix: Ix) -> String {
    let tree = &m.tree;
    let mut parts = vec![];
    if tree.flags(ix).has(NodeFlags::DUP_MOUNT) {
        parts.push("mounted again: counted at its other mount".to_string());
    } else if tree.dir(ix).is_some_and(|d| d.state == DirState::Skipped) {
        parts.push("other filesystem, not scanned (--cross-mounts)".to_string());
    }
    if let Some(l) = dut_tui::subvol_label(m, ix) {
        parts.push(l);
    }
    if tree.flags(ix).has(NodeFlags::COMPRESSED) {
        parts.push("compressed: at most this".into());
    }
    let out = tree.total(ix).shared_out;
    if out > 0 {
        parts.push(format!(
            "{} also used outside the scan",
            dut_tui::human(out)
        ));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("  [{}]", parts.join("; "))
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
    if t.referenced > t.alloc {
        let why = if m.status.exact_sizes {
            "shared data counted once, compressed data at its size on disk"
        } else {
            "shared data counted once"
        };
        eprintln!(
            "{} less than du ({}): {why} (--like-du for du's number)",
            dut_tui::human(t.referenced - t.alloc),
            dut_tui::human(t.referenced)
        );
    }
    if let Some((name, ratio)) = m.status.profile
        && ratio != 1.0
    {
        eprintln!("btrfs data profile {name}: each byte is stored {ratio:.1} times");
    }
    if m.status.compressed_upper_bound {
        eprintln!(
            "compressed files are shown at their uncompressed size, an upper bound; run as \
             root for sizes on disk"
        );
    }
    if m.status.unmeasured > 0 {
        eprintln!(
            "{} files could not be measured; data they share may be counted more than once",
            m.status.unmeasured
        );
    }
    if s.corrupt + s.lost + s.duplicate > 0 {
        eprintln!(
            "pipeline: corrupt {} lost {} duplicate {}",
            s.corrupt, s.lost, s.duplicate
        );
    }
}

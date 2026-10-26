use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::{Args, Parser, Subcommand};
use dut_engine::{Command, Engine, EngineOptions, Model, Timing};
use dut_scan::{Backend, JobOptions, PortableBackend, native_backend};
use dut_tree::{DirState, Ix, NodeFlags, Tree};
use dut_tui::SizeMode;

#[derive(Parser)]
#[command(version, about, args_conflicts_with_subcommands = true)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
    /// Directory to browse (opens the interactive browser when stdout is a terminal).
    path: Option<PathBuf>,
    #[command(flatten)]
    mounts: MountArgs,
    /// Choose a disk to browse from the mounted filesystems.
    #[arg(long, conflicts_with = "path")]
    disks: bool,
    /// Never delete anything (the delete keys are refused).
    #[arg(long)]
    read_only: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// Scan a directory and print a summary.
    Scan(ScanArgs),
    /// List the mounted filesystems with their size and free space.
    Disks,
}

/// Which mounts below the scanned directory are descended into. Kernel filesystems (`/proc`,
/// `/sys`, cgroups) never are.
#[derive(Args, Clone, Copy, Default)]
struct MountArgs {
    /// Stay on the filesystem of the scanned directory (the default; overrides --cross-mounts
    /// and --network given before it).
    #[arg(short = 'x', long, overrides_with = "cross_mounts")]
    one_file_system: bool,
    /// Descend into other local filesystems (disks, tmpfs).
    #[arg(long, overrides_with = "one_file_system")]
    cross_mounts: bool,
    /// Descend into network filesystems (NFS, SMB, sshfs and other FUSE network mounts); with
    /// or without --cross-mounts.
    #[arg(long, overrides_with = "one_file_system")]
    network: bool,
}

impl MountArgs {
    fn job(self) -> JobOptions {
        JobOptions {
            cross_mounts: self.cross_mounts && !self.one_file_system,
            network: self.network && !self.one_file_system,
            ..Default::default()
        }
    }
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
    #[command(flatten)]
    mounts: MountArgs,
    /// Print only the root total in bytes (for scripts and tests).
    #[arg(long)]
    bytes: bool,
    /// List every entry that could not be read, with the reason.
    #[arg(long)]
    errors: bool,
    /// Abandon directories stalled this many seconds.
    #[arg(long, default_value_t = 30)]
    stall_abandon: u64,
    /// Print pipeline counters and applier timing (lock hold and wait) after the scan.
    #[arg(long, hide = true)]
    debug_stats: bool,
    /// Filesystem access: `native` (the fastest for this system) or `portable` (std::fs, the
    /// reference the native backends are tested against).
    #[arg(long, value_enum, default_value_t = BackendArg::Native, hide = true)]
    backend: BackendArg,
}

#[derive(clap::ValueEnum, Clone, Copy)]
enum BackendArg {
    Native,
    Portable,
}

/// `println!` that ends the process quietly when stdout is closed (`dut scan | head`), instead
/// of panicking.
macro_rules! outln {
    ($($t:tt)*) => {{
        use std::io::Write as _;
        if let Err(e) = writeln!(std::io::stdout(), $($t)*)
            && e.kind() == std::io::ErrorKind::BrokenPipe
        {
            std::process::exit(0);
        }
    }};
}

/// Exit codes from `docs/research/safety-persistence-ops.md` §4.3.
const EXIT_PARTIAL: u8 = 3;

/// Writes a crash report for a panic on any thread, with the engine's counters and last
/// messages, then runs the previous hook. In the browser, `dut_tui::run` wraps this hook in one
/// that restores the terminal first.
fn install_crash_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(dir) = dut_engine::default_crash_dir() {
            let thread = std::thread::current();
            let report = format!(
                "dut {} crash report\n\nthread: {}\npanic: {info}\n\nbacktrace:\n{}\n\n{}",
                env!("CARGO_PKG_VERSION"),
                thread.name().unwrap_or("?"),
                std::backtrace::Backtrace::force_capture(),
                dut_engine::crash_state().unwrap_or_default(),
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
        Some(Cmd::Disks) => disks(),
        None if cli.disks && !std::io::stdout().is_terminal() => disks(),
        None => {
            let path = cli.path.clone().unwrap_or_else(|| PathBuf::from("."));
            if !std::io::stdout().is_terminal() {
                return scan(ScanArgs {
                    path,
                    depth: 1,
                    top: 10,
                    apparent: false,
                    like_du: false,
                    threads: None,
                    measure_workers: None,
                    mounts: cli.mounts,
                    bytes: false,
                    errors: false,
                    stall_abandon: 30,
                    debug_stats: false,
                    backend: BackendArg::Native,
                });
            }
            let opts = dut_tui::RunOptions {
                path: (!cli.disks).then_some(path),
                job: cli.mounts.job(),
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

/// `dut disks`: one line per filesystem, like `df -h`.
fn disks() -> ExitCode {
    let list = match dut_sys::disks(dut_tui::DISK_TIMEOUT) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("dut: cannot list disks: {e}");
            return ExitCode::from(1);
        }
    };
    let width = list
        .iter()
        .map(|d| d.mount_point.as_os_str().len())
        .max()
        .unwrap_or(0)
        .clamp(10, 40);
    outln!(
        "{:<width$}  {:<8} {:>10} {:>10} {:>10}  {:>4}",
        "MOUNTED ON",
        "TYPE",
        "SIZE",
        "USED",
        "FREE",
        "USE%"
    );
    for d in &list {
        let mut line = format!("{:<width$}  {:<8}", d.mount_point.display(), d.fstype);
        match d.space {
            Some(s) => {
                line += &format!(
                    " {:>10} {:>10} {:>10}  {:>3.0}%",
                    dut_tui::human(s.total),
                    dut_tui::human(s.used),
                    dut_tui::human(s.available),
                    s.used as f64 * 100.0 / s.total.max(1) as f64
                );
            }
            None => line += "  not responding",
        }
        if d.class == dut_sys::FsClass::Network {
            line += "  network";
        }
        if !d.also_at.is_empty() {
            let also: Vec<String> = d.also_at.iter().map(|p| p.display().to_string()).collect();
            line += &format!("  also at {}", also.join(", "));
        }
        outln!("{line}");
    }
    ExitCode::SUCCESS
}

fn scan(args: ScanArgs) -> ExitCode {
    let mut opts = EngineOptions {
        auto_abandon: Some(Duration::from_secs(args.stall_abandon)),
        ..Default::default()
    };
    let storage = dut_sys::storage(&args.path);
    opts.workers = args
        .threads
        .unwrap_or_else(|| dut_engine::workers_for(storage));
    if let Some(n) = args.measure_workers {
        opts.measure_workers = n;
    }
    if args.debug_stats {
        eprintln!("storage: {storage:?}, {} workers", opts.workers);
    }
    match args.backend {
        BackendArg::Native => scan_with(Engine::start(native_backend(storage), opts), args),
        BackendArg::Portable => scan_with(Engine::start(PortableBackend::default(), opts), args),
    }
}

fn scan_with<B: Backend>(engine: Engine<B>, args: ScanArgs) -> ExitCode {
    let ScanArgs {
        path,
        depth,
        top,
        apparent,
        like_du,
        mounts,
        bytes,
        errors: list_errors,
        debug_stats,
        ..
    } = args;
    let t0 = Instant::now();
    engine.send(Command::Scan {
        path,
        opts: mounts.job(),
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
            outln!("{}", mode.of(&t));
        } else {
            print_tree(m, root, depth, top, mode);
            if list_errors {
                print_errors(m, root);
            }
            print_footer(m, root, elapsed, list_errors);
        }
        if debug_stats {
            print_debug_stats(m);
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
    outln!(
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
            outln!(
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
            outln!(
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
    if let Some(p) = dut_tui::problem(tree, ix) {
        parts.push(p);
    }
    match dut_tui::errors_below(tree, ix) {
        0 => {}
        1 => parts.push("1 error inside".into()),
        n => parts.push(format!("{n} errors inside")),
    }
    if tree.flags(ix).has(NodeFlags::DUP_MOUNT) {
        parts.push("mounted again: counted at its other mount".to_string());
    } else if tree.flags(ix).has(NodeFlags::KERNEL) {
        parts.push("kernel filesystem, never scanned".to_string());
    } else if tree.flags(ix).has(NodeFlags::NETWORK) {
        parts.push("network filesystem, not scanned (--network)".to_string());
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

/// Every entry that could not be read, in path order.
fn print_errors(m: &Model, root: Ix) {
    let list = dut_tui::errors(&m.tree, root, usize::MAX);
    if list.items.is_empty() {
        return;
    }
    outln!();
    outln!("Not read:");
    for e in &list.items {
        outln!(
            "  {}{}: {}",
            e.path.display(),
            if e.is_dir { "/" } else { "" },
            e.problem
        );
    }
}

fn print_footer(m: &Model, root: Ix, elapsed: Duration, listed: bool) {
    let t = m.tree.total(root);
    let s = &m.status.stats;
    eprintln!(
        "{} items, {}, {:.2}s{}",
        t.items,
        match t.errors {
            0 => "no errors".to_string(),
            1 => "1 error".to_string(),
            n => format!("{n} errors"),
        },
        elapsed.as_secs_f64(),
        if m.status.incomplete {
            ", incomplete"
        } else {
            ""
        }
    );
    if t.errors > 0 {
        let list = dut_tui::errors(&m.tree, root, 0);
        let mut line = format!("not read: {}", list.summary());
        if let Some(h) = list.hint() {
            line += &format!("; {h}");
        }
        if !listed {
            line += "; --errors lists them";
        }
        eprintln!("{line}");
    }
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

/// `--debug-stats`: what went through the pipeline and how the applier spent its time (D1:
/// entries applied per second of lock time, hold and wait times per slice).
fn print_debug_stats(m: &Model) {
    let s = &m.status.stats;
    let t = &m.status.timing;
    eprintln!(
        "pipeline: {} messages, {} chunks, {} closes, {} entries; discarded {} (job) {} (cut); \
         late {}, stalled {}",
        s.messages,
        s.chunks,
        s.closes,
        s.entries,
        s.discarded_job,
        s.discarded_cut,
        s.late,
        s.stalled
    );
    let (names, garbage) = m.tree.name_bytes();
    eprintln!(
        "tree: {} nodes, {:.1} MiB of names ({:.1} MiB freed)",
        m.tree.live_nodes(),
        names as f64 / 1048576.0,
        garbage as f64 / 1048576.0
    );
    let locked = Duration::from_nanos(t.locked_ns);
    let rate = s.entries as f64 / locked.as_secs_f64().max(1e-9);
    let max = |h: &[u64; Timing::BUCKETS]| {
        h.iter()
            .rposition(|&n| n > 0)
            .map_or(Duration::ZERO, |k| Duration::from_micros(1 << k))
    };
    eprintln!(
        "applier: {:.0} ms holding the write lock in {} slices, {:.2}M entries/s; hold p50 < {:?}, \
         p99 < {:?}, max < {:?}; wait for the lock p99 < {:?}, max < {:?}",
        locked.as_secs_f64() * 1000.0,
        t.slices,
        rate / 1e6,
        Timing::percentile(&t.hold, 0.5),
        Timing::percentile(&t.hold, 0.99),
        max(&t.hold),
        Timing::percentile(&t.wait, 0.99),
        max(&t.wait),
    );
}

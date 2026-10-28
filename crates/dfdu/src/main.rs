use std::ffi::OsStr;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

mod clean;
mod history;
mod inject;
mod json;
mod report;

use clap::{Args, Parser, Subcommand};
use dfdu_engine::{Command, Engine, EngineOptions, Model, Timing};
use dfdu_rules::RuleSet;
use dfdu_scan::classify::ClassOpts;
use dfdu_scan::{Backend, JobOptions, PortableBackend, native_backend};
use dfdu_tree::filetype::{self, TypeFilter};
use dfdu_tree::{DirState, Ix, NodeFlags, Tree};
use dfdu_tui::{ColorLevel, SizeMode, Theme, ThemeChoice, ThemeFile, UiSettings};

/// The exit codes, for `--help` and the man page (architecture §8).
const EXIT_CODES: &str = "Exit codes:
  0    success
  1    fatal error (the directory could not be read, an output could not be written)
  2    usage error
  3    finished, but some entries could not be read or the scan is incomplete
  4    report --fail-above: the matches add up to more than the limit
  5    clean, recover: some deletions failed or were refused
  130  interrupted (Ctrl-C)";

#[derive(Parser)]
#[command(
    version,
    about,
    args_conflicts_with_subcommands = true,
    after_long_help = EXIT_CODES
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
    /// Directory to browse (opens the interactive browser when stdout is a terminal).
    path: Option<PathBuf>,
    #[command(flatten)]
    mounts: MountArgs,
    #[command(flatten)]
    rules: RuleArgs,
    /// Choose a disk to browse from the mounted filesystems.
    #[arg(long, conflicts_with = "path")]
    disks: bool,
    /// Open the scan saved in FILE (a snapshot or an ncdu export) at once, and rescan its
    /// directory in the background, replacing it when done.
    #[arg(long, value_name = "FILE", conflicts_with_all = ["path", "disks", "import"])]
    resume_from: Option<PathBuf>,
    /// Browse a scan saved in FILE (a snapshot, an ncdu JSON export, plain or zstd, or an ncdu
    /// binary export from `ncdu -O`; `-` for stdin), read-only.
    #[arg(short = 'f', long = "file", value_name = "FILE", conflicts_with_all = ["path", "disks"])]
    import: Option<PathBuf>,
    /// Never delete anything (the delete keys are refused).
    #[arg(long)]
    read_only: bool,
    /// Show only files of these types or extensions, comma-separated: `video,audio,.iso`
    /// (`t` in the browser changes it).
    #[arg(long = "type", value_name = "TYPES", value_parser = type_filter)]
    types: Option<TypeFilter>,
    /// Do not record the scan in the history, nor show what grew since the last one.
    #[arg(long)]
    no_history: bool,
    /// Colour theme: auto, dark, light, ansi (the terminal's own 16 colours), mono, high-contrast,
    /// or NAME for the theme file NAME.toml in dfdu/themes in the configuration directory. auto
    /// asks the terminal for its background colour at start (up to 1 s if it does not answer)
    /// and picks dark or light, or ansi when the colour is unknown.
    #[arg(long, value_name = "THEME", default_value = "auto")]
    theme: String,
    /// Colours: auto (from the terminal and NO_COLOR), never, 16, 256 or truecolor. A level given
    /// here is used even when NO_COLOR is set.
    #[arg(long, value_name = "WHEN", default_value = "auto", value_parser = color_flag)]
    color: ColorFlag,
    /// Do not capture the mouse (click to select, double-click to open, wheel to scroll). While
    /// it is captured, most terminals select text with Shift held down.
    #[arg(long)]
    no_mouse: bool,
    /// Start with the debug panel open, and print its numbers when the browser exits.
    #[arg(long, hide = true)]
    debug_stats: bool,
}

/// A `--color` value; `None` detects the level.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ColorFlag(Option<ColorLevel>);

fn color_flag(s: &str) -> Result<ColorFlag, String> {
    dfdu_tui::parse_color_flag(s).map(ColorFlag)
}

/// The theme `--theme` names: `auto`, a built-in theme, or the theme file `NAME.toml` in
/// `themes` (`None` when the platform has no configuration directory).
fn theme_choice(name: &str, themes: Option<&Path>) -> Result<ThemeChoice, String> {
    if let Some(choice) = ThemeChoice::builtin(name) {
        return Ok(choice);
    }
    let builtin = format!("auto, {}", Theme::builtin_names().join(", "));
    if Path::new(name).file_name() != Some(OsStr::new(name)) {
        return Err(format!(
            "not a built-in theme ({builtin}) or the name of a theme file"
        ));
    }
    let Some(themes) = themes else {
        return Err(format!(
            "not a built-in theme ({builtin}), and there is no configuration directory for theme \
             files"
        ));
    };
    let path = themes.join(format!("{name}.toml"));
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "not a built-in theme ({builtin}), and there is no file {}",
                path.display()
            ));
        }
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let mut file = ThemeFile::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    file.name.get_or_insert_with(|| name.to_owned());
    Ok(ThemeChoice::File(file))
}

#[derive(Subcommand)]
enum Cmd {
    /// Scan a directory and print a summary.
    Scan(ScanArgs),
    /// List the mounted filesystems with their size and free space.
    Disks,
    /// Scan a directory and list what the cleanup rules found in it: caches, build output,
    /// dependencies, with how safe each is to delete.
    Report(report::ReportArgs),
    /// Delete what `report` recommends (dry run unless --yes).
    Clean(clean::CleanArgs),
    /// What grew: a directory now against its last recorded scan (or one from --since ago),
    /// or two saved scans.
    Diff(history::DiffArgs),
    /// List the recorded scans.
    History(history::HistoryArgs),
    /// Scan a directory and list the files with identical contents (size, then partial hash,
    /// then SHA-256), the most space to free first.
    Dupes(dupes::DupesArgs),
    /// Scan a directory and write it in ncdu's JSON format.
    Export(ExportArgs),
    /// Read a scan in ncdu's JSON format and print a summary (`dfdu -f FILE` browses it).
    Import(ImportArgs),
    /// List what earlier sessions left unfinished (staged items, interrupted deletes), and
    /// finish, restore or dismiss them.
    Recover(RecoverArgs),
    /// Write man pages (dfdu.1 and one per command) into DIR, for packagers.
    #[command(hide = true)]
    Man { dir: PathBuf },
    /// Print a shell completion script (bash, zsh, fish, elvish, powershell).
    Completions { shell: clap_complete::Shell },
}

/// `dfdu man DIR`: `dfdu.1` and `dfdu-<command>.1` for each visible command.
fn man(dir: &Path) -> ExitCode {
    use clap::CommandFactory;
    let write = || -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let cmd = Cli::command();
        let mut out = std::fs::File::create(dir.join("dfdu.1"))?;
        clap_mangen::Man::new(cmd.clone()).render(&mut out)?;
        for sub in cmd.get_subcommands().filter(|s| !s.is_hide_set()) {
            let name: &'static str = format!("dfdu-{}", sub.get_name()).leak();
            let page = sub.clone().name(name).bin_name(name.replacen("-", " ", 1));
            let mut out = std::fs::File::create(dir.join(format!("{name}.1")))?;
            clap_mangen::Man::new(page).render(&mut out)?;
        }
        Ok(())
    };
    match write() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("dfdu: {}: {e}", dir.display());
            ExitCode::from(1)
        }
    }
}

#[derive(Args)]
struct RecoverArgs {
    /// Delete what is left of these staged items (numbers from the list).
    #[arg(long, value_name = "N", value_delimiter = ',')]
    finish: Vec<usize>,
    /// Put these staged items back at their paths (next to them as NAME.restored-N if
    /// something is there now), or take these items found in the trash back out of it.
    #[arg(long, value_name = "N", value_delimiter = ',')]
    restore: Vec<usize>,
    /// Close these interrupted items: nothing is done on disk.
    #[arg(long, value_name = "N", value_delimiter = ',')]
    dismiss: Vec<usize>,
    /// The journal directory (the default is dfdu's state directory).
    #[arg(long, hide = true)]
    journals: Option<PathBuf>,
}

#[derive(Args)]
struct ExportArgs {
    #[arg(default_value = ".")]
    path: PathBuf,
    /// Where to write (`-` for stdout).
    #[arg(short, long, value_name = "FILE", default_value = "-")]
    output: PathBuf,
    /// Compress with zstd (ncdu reads it with `-f`; dfdu recognises it on import).
    #[arg(long, conflicts_with = "snapshot")]
    compress: bool,
    /// Write dfdu's own snapshot format instead (lossless, checked, compressed; for
    /// `dfdu -f` and `--resume-from`, not for ncdu).
    #[arg(long)]
    snapshot: bool,
    /// Worker threads.
    #[arg(long)]
    threads: Option<usize>,
    /// Start from this snapshot (`--snapshot`) and rescan only the directories changed since
    /// it was taken, when the volume's change journal can tell (macOS); otherwise everything.
    /// PATH and the mount options are the snapshot's.
    #[arg(long, value_name = "SNAPSHOT")]
    resume_from: Option<PathBuf>,
    #[command(flatten)]
    mounts: MountArgs,
}

#[derive(Args)]
struct ImportArgs {
    /// The export (`-` for stdin).
    file: PathBuf,
    /// Levels of directories to print below the root.
    #[arg(long, default_value_t = 1)]
    depth: usize,
    /// Entries to print per directory, largest first.
    #[arg(long, default_value_t = 10)]
    top: usize,
    /// Show apparent sizes instead of disk usage.
    #[arg(long)]
    apparent: bool,
    /// Count only files of these types or extensions, comma-separated: `video,audio,.iso`.
    /// Types: video, audio, image, document, archive, disk-image, code, executable, font, data,
    /// other. Folders show the size of the matching files inside them.
    #[arg(long = "type", value_name = "TYPES", value_parser = type_filter)]
    types: Option<TypeFilter>,
    /// After the tree, print disk usage by file type and the largest extensions.
    #[arg(long)]
    by_type: bool,
    /// Print only the root total in bytes.
    #[arg(long)]
    bytes: bool,
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

/// Classification of what the scan finds (categories and cleanup rules).
#[derive(Args, Clone, Copy, Default)]
struct RuleArgs {
    /// Do not classify entries: no categories, no cleanup candidates.
    #[arg(long)]
    no_rules: bool,
}

impl RuleArgs {
    /// The built-in rules with the user's rule files (dfdu/rules/*.toml in the configuration
    /// directory). A rule that does not load is reported and left out.
    fn load(self) -> Option<Arc<RuleSet>> {
        if self.no_rules {
            return None;
        }
        let dir = dfdu_tui::rules_dir();
        let (set, errors) = dfdu_rules::load(&dfdu_rules::Env::current(), dir.as_deref());
        for e in errors {
            eprintln!("dfdu: {e}");
        }
        Some(Arc::new(set))
    }
}

#[derive(Args)]
struct ScanArgs {
    #[arg(default_value = ".")]
    path: PathBuf,
    /// Output: table (for people), json (one document, schema/scan.v1.json), ndjson (one
    /// record per line, for large trees) or ncdu (ncdu's JSON export).
    #[arg(long, value_enum, default_value_t = Format::Table)]
    format: Format,
    /// Levels of directories to print below the root [default: 1; every level for ndjson].
    #[arg(long)]
    depth: Option<usize>,
    /// Entries to print per directory, largest first [default: 10; all for ndjson].
    #[arg(long)]
    top: Option<usize>,
    /// Show apparent sizes instead of disk usage.
    #[arg(long, conflicts_with = "like_du")]
    apparent: bool,
    /// Count data shared between files (btrfs snapshots, reflinks, APFS clones) once per file,
    /// as du does, instead of once.
    #[arg(long)]
    like_du: bool,
    /// Worker threads for the scanned filesystem. Mounts below it with their own limit (spinning
    /// disks: 2, network: 4) add that many.
    #[arg(long)]
    threads: Option<usize>,
    /// Threads measuring shared extents (btrfs, XFS; APFS clones of 1 MiB or more); 0 reports
    /// sizes as du does.
    #[arg(long)]
    measure_workers: Option<usize>,
    #[command(flatten)]
    mounts: MountArgs,
    #[command(flatten)]
    rules: RuleArgs,
    /// Count only files of these types or extensions, comma-separated: `video,audio,.iso`.
    /// Types: video, audio, image, document, archive, disk-image, code, executable, font, data,
    /// other. Folders show the size of the matching files inside them.
    #[arg(long = "type", value_name = "TYPES", value_parser = type_filter)]
    types: Option<TypeFilter>,
    /// After the tree, print disk usage by file type and the largest extensions.
    #[arg(long)]
    by_type: bool,
    /// Print only the root total in bytes (for scripts and tests).
    #[arg(long)]
    bytes: bool,
    /// List every entry that could not be read, with the reason.
    #[arg(long)]
    errors: bool,
    /// Do not record the scan in the history (`dfdu diff`, `dfdu history`).
    #[arg(long)]
    no_history: bool,
    /// Abandon directories stalled this many seconds.
    #[arg(long, default_value_t = 30.0, value_parser = seconds)]
    stall_abandon: f64,
    /// Report a directory as stalled after one call on it took this many seconds (tests).
    #[arg(long, default_value_t = 10.0, value_parser = seconds, hide = true)]
    stall_after: f64,
    /// Print pipeline counters and applier timing (lock hold and wait) after the scan.
    #[arg(long, hide = true)]
    debug_stats: bool,
    /// Filesystem access: `native` (the fastest for this system) or `portable` (std::fs, the
    /// reference the native backends are tested against).
    #[arg(long, value_enum, default_value_t = BackendArg::Native, hide = true)]
    backend: BackendArg,
}

#[derive(clap::ValueEnum, Clone, Copy, PartialEq)]
enum Format {
    Table,
    Json,
    Ndjson,
    Ncdu,
}

#[derive(clap::ValueEnum, Clone, Copy)]
enum BackendArg {
    Native,
    Portable,
}

/// A non-negative number of seconds, at most a year.
fn seconds(s: &str) -> Result<f64, String> {
    match s.parse::<f64>() {
        Ok(v) if (0.0..=31_536_000.0).contains(&v) => Ok(v),
        _ => Err(format!("`{s}` is not a number of seconds")),
    }
}

/// `println!` that ends the process quietly when stdout is closed (`dfdu scan | head`), instead
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

// After `outln!`, which it uses.
mod dupes;

/// Exit code when an entry could not be read or the scan is incomplete.
const EXIT_PARTIAL: u8 = 3;
/// Exit code when some deletions failed or were refused.
const EXIT_DELETE_FAILED: u8 = 5;

/// Writes a crash report for a panic on any thread, with the engine's counters and last
/// messages, then runs the previous hook. In the browser, `dfdu_tui::run` wraps this hook in one
/// that restores the terminal first. Panics the engine catches (a scan worker's, which becomes a
/// `Panicked` directory and an error in the scan's result, and the applier's, for which the
/// engine writes its own report) are left to the engine: no file, no message.
fn install_crash_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if dfdu_engine::panic_is_caught_by_engine(std::thread::current().name()) {
            return;
        }
        if let Some(dir) = dfdu_engine::default_crash_dir() {
            let thread = std::thread::current();
            let report = format!(
                "dfdu {} crash report\n\nthread: {}\npanic: {info}\n\nbacktrace:\n{}\n\n{}",
                env!("CARGO_PKG_VERSION"),
                thread.name().unwrap_or("?"),
                std::backtrace::Backtrace::force_capture(),
                dfdu_engine::crash_state().unwrap_or_default(),
            );
            if let Some(p) = dfdu_engine::write_crash_report(&dir, &report) {
                eprintln!("dfdu: crash report written to {}", p.display());
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
        Some(Cmd::Report(args)) => match (RuleArgs::default()).load() {
            Some(rules) => report::run(args, rules),
            None => ExitCode::from(1),
        },
        Some(Cmd::Clean(args)) => match (RuleArgs::default()).load() {
            Some(rules) => clean::run(args, rules),
            None => ExitCode::from(1),
        },
        Some(Cmd::Diff(args)) => history::diff(args),
        Some(Cmd::History(args)) => history::list(args),
        Some(Cmd::Dupes(args)) => dupes::run(args),
        Some(Cmd::Export(args)) => export(args),
        Some(Cmd::Import(args)) => import(args),
        Some(Cmd::Recover(args)) => recover(args),
        Some(Cmd::Man { dir }) => man(&dir),
        Some(Cmd::Completions { shell }) => {
            use clap::CommandFactory;
            clap_complete::generate(shell, &mut Cli::command(), "dfdu", &mut std::io::stdout());
            ExitCode::SUCCESS
        }
        None if cli.disks && !std::io::stdout().is_terminal() => disks(),
        None => {
            let path = cli.path.unwrap_or_else(|| PathBuf::from("."));
            if !std::io::stdout().is_terminal() {
                return scan(ScanArgs {
                    path,
                    format: Format::Table,
                    depth: None,
                    top: None,
                    apparent: false,
                    like_du: false,
                    threads: None,
                    measure_workers: None,
                    mounts: cli.mounts,
                    rules: cli.rules,
                    bytes: false,
                    types: cli.types,
                    by_type: false,
                    errors: false,
                    no_history: false,
                    stall_abandon: 30.0,
                    stall_after: 10.0,
                    debug_stats: false,
                    backend: BackendArg::Native,
                });
            }
            let theme = match theme_choice(&cli.theme, dfdu_tui::theme_dir().as_deref()) {
                Ok(theme) => theme,
                Err(e) => {
                    eprintln!("dfdu: theme {}: {e}", cli.theme);
                    return ExitCode::from(2);
                }
            };
            let imported = match &cli.import {
                Some(f) => match read_export(f) {
                    Ok(im) => Some(im.tree),
                    Err(code) => return code,
                },
                None => None,
            };
            let (resumed, journal) = match &cli.resume_from {
                Some(f) => match read_export(f) {
                    Ok(im) => (Some(im.tree), im.journal),
                    Err(code) => return code,
                },
                None => (None, None),
            };
            let opts = dfdu_tui::RunOptions {
                path: (!cli.disks).then_some(path),
                imported,
                resumed,
                journal,
                job: cli.mounts.job(),
                rules: cli.rules.load(),
                engine: EngineOptions {
                    read_only: cli.read_only,
                    ..Default::default()
                },
                history: !cli.no_history && cli.import.is_none(),
                ui: UiSettings {
                    theme,
                    color: cli.color.0,
                    mouse: !cli.no_mouse,
                    debug_stats: cli.debug_stats,
                    types: cli.types.unwrap_or_default(),
                },
            };
            match dfdu_tui::run(opts) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("dfdu: {e}");
                    ExitCode::from(1)
                }
            }
        }
    }
}

/// Where the change journal of `path`'s volume stands (FSEvents on macOS); `None` where there
/// is none.
fn journal_position(path: &Path) -> Option<dfdu_store::snapshot::Journal> {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::MetadataExt;
        let dev = std::fs::metadata(path).ok()?.dev() as u64;
        let volume = dfdu_sys::fsevents::volume_uuid(dev)?;
        Some(dfdu_store::snapshot::Journal {
            volume,
            event_id: dfdu_sys::fsevents::current_event_id(),
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        None
    }
}

/// Reads a snapshot or an ncdu export from `file` (`-` for stdin), reporting a failure.
fn read_export(file: &Path) -> Result<dfdu_store::Opened, ExitCode> {
    let limits = dfdu_store::ncdu::Limits::default();
    let r = if file == Path::new("-") {
        dfdu_store::open(std::io::stdin().lock(), &limits)
    } else {
        match std::fs::File::open(file) {
            Ok(f) => dfdu_store::open_file(f, &limits),
            Err(e) => {
                eprintln!("dfdu: {}: {e}", file.display());
                return Err(ExitCode::from(1));
            }
        }
    };
    r.map_err(|e| {
        eprintln!("dfdu: {}: {e}", file.display());
        ExitCode::from(1)
    })
}

/// `dfdu recover`: lists the unfinished items, numbered from 1, then does what was asked
/// for each, highest number first so the numbers stay valid.
fn recover(args: RecoverArgs) -> ExitCode {
    let Some(dir) = args.journals.or_else(dfdu_clean::default_journal_dir) else {
        eprintln!("dfdu: no state directory for the deletion journal");
        return ExitCode::from(1);
    };
    let mut r = match dfdu_clean::recover::scan(&dir) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("dfdu: {}: {e}", dir.display());
            return ExitCode::from(1);
        }
    };
    for n in &r.notes {
        eprintln!("dfdu: {n}");
    }
    let mut todo: Vec<(usize, &str)> = args
        .finish
        .iter()
        .map(|&n| (n, "finish"))
        .chain(args.restore.iter().map(|&n| (n, "restore")))
        .chain(args.dismiss.iter().map(|&n| (n, "dismiss")))
        .collect();
    if todo.is_empty() {
        if r.items.is_empty() {
            outln!("nothing unfinished");
        }
        for (i, u) in r.items.iter().enumerate() {
            outln!("{:>3}  {}", i + 1, u.describe());
        }
        return ExitCode::SUCCESS;
    }
    todo.sort_by_key(|&(n, _)| std::cmp::Reverse(n));
    if todo.windows(2).any(|w| w[0].0 == w[1].0) {
        eprintln!("dfdu: an item is named twice");
        return ExitCode::from(2);
    }
    let mut failed = false;
    for (n, what) in todo {
        let Some(i) = n.checked_sub(1).filter(|&i| i < r.items.len()) else {
            eprintln!("dfdu: no item {n}");
            failed = true;
            continue;
        };
        let path = r.items[i].path.display().to_string();
        let res = match what {
            "finish" => r.finish(i).map(|()| format!("{path}: deleted")),
            "restore" => r
                .restore(i)
                .map(|to| format!("{path}: restored at {}", to.display())),
            _ => r.dismiss(i).map(|()| format!("{path}: dismissed")),
        };
        match res {
            Ok(msg) => outln!("{msg}"),
            Err(e) => {
                eprintln!("dfdu: {path}: {e}");
                failed = true;
            }
        }
    }
    if failed {
        ExitCode::from(EXIT_DELETE_FAILED)
    } else {
        ExitCode::SUCCESS
    }
}

/// `dfdu import`: the summary `dfdu scan` prints, for a scan read from a file.
fn import(args: ImportArgs) -> ExitCode {
    let im = match read_export(&args.file) {
        Ok(im) => im,
        Err(code) => return code,
    };
    let m = Model {
        tree: im.tree,
        ..Model::default()
    };
    let mode = if args.apparent {
        SizeMode::Apparent
    } else {
        SizeMode::Disk
    };
    let total = m.tree.total(im.root);
    let filter = args.types.as_ref();
    if args.bytes {
        outln!("{}", mode.of(&shown_total(&m.tree, im.root, filter)));
    } else {
        print_tree(&m, im.root, args.depth, args.top, mode, filter);
        if args.by_type {
            print_by_type(&m.tree, im.root, mode, filter);
        }
        let by = im
            .written_by
            .map(|w| format!(", written by {w}"))
            .unwrap_or_default();
        outln!("{} entries{by}", im.entries);
    }
    if total.errors > 0 {
        ExitCode::from(EXIT_PARTIAL)
    } else {
        ExitCode::SUCCESS
    }
}

/// `dfdu export`: scans `path` and writes it in ncdu's format.
fn export(args: ExportArgs) -> ExitCode {
    let resumed = match &args.resume_from {
        Some(f) => {
            let read = std::fs::File::open(f)
                .map_err(dfdu_store::ImportError::from)
                .and_then(|r| {
                    dfdu_store::snapshot::read(std::io::BufReader::new(r), &Default::default())
                });
            match read {
                Ok(l) => Some(l),
                Err(e) => {
                    eprintln!("dfdu: {}: {e}", f.display());
                    return ExitCode::from(1);
                }
            }
        }
        None => None,
    };
    let mut job = args.mounts.job();
    let path = match &resumed {
        Some(l) => {
            job.cross_mounts = l.header.cross_mounts;
            job.network = l.header.network;
            l.tree.path_buf(l.root)
        }
        None => args.path.clone(),
    };
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
    let engine = Engine::start(native_backend(storage), opts);
    // Taken before the scan, so that what changes during it is replayed on resume.
    let journal = journal_position(&path);
    match resumed {
        Some(l) => {
            let changed = l
                .header
                .journal
                .and_then(|j| dfdu_engine::changed_since(&path, j.volume, j.event_id));
            let node = l.tree.node_ref(l.root);
            engine.load(l.tree);
            engine.send(Command::Resume {
                node,
                opts: job.clone(),
                changed,
            });
        }
        None => engine.send(Command::Scan {
            path: path.clone(),
            opts: job.clone(),
        }),
    }
    engine.wait_idle(None);
    if let Some(msg) = engine.read(|m| m.status.fatal.clone()) {
        eprintln!("dfdu: {msg}");
        return ExitCode::from(1);
    }
    let meta = ncdu_meta();
    let to_stdout = args.output == Path::new("-");
    let result = engine.read(|m| {
        let root = m
            .tree
            .roots()
            .next()
            .ok_or_else(|| std::io::Error::other("no scan"))?;
        let partial = m.tree.total(root).errors > 0 || m.status.incomplete;
        let write = |w: &mut dyn std::io::Write| -> std::io::Result<_> {
            if args.snapshot {
                let header = dfdu_store::snapshot::Header {
                    tool_version: env!("CARGO_PKG_VERSION").into(),
                    written: meta.timestamp.unwrap_or(0),
                    cross_mounts: job.cross_mounts,
                    network: job.network,
                    journal,
                };
                let entries = dfdu_store::snapshot::write(&m.tree, root, w, &header)?;
                Ok(dfdu_store::ncdu::ExportStats {
                    entries,
                    non_utf8: 0,
                })
            } else if args.compress {
                let mut z = dfdu_store::zstd_writer(w)?;
                let s = dfdu_store::ncdu::export(&m.tree, root, &mut z, &meta)?;
                z.finish()?;
                Ok(s)
            } else {
                dfdu_store::ncdu::export(&m.tree, root, w, &meta)
            }
        };
        let stats = if to_stdout {
            write(&mut std::io::stdout().lock())?
        } else {
            dfdu_store::write_atomic(&args.output, |f| write(f))?
        };
        Ok::<_, std::io::Error>((stats, partial))
    });
    engine.shutdown(Duration::from_millis(500));
    match result {
        Ok((stats, partial)) => {
            if stats.non_utf8 > 0 {
                eprintln!(
                    "dfdu: {} names are not UTF-8 and were written as they are; ncdu reads \
                     them, most JSON tools do not",
                    stats.non_utf8
                );
            }
            if partial {
                ExitCode::from(EXIT_PARTIAL)
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("dfdu: {}: {e}", args.output.display());
            ExitCode::from(1)
        }
    }
}

/// What an ncdu export says about its writer.
fn ncdu_meta() -> dfdu_store::ncdu::Metadata {
    dfdu_store::ncdu::Metadata {
        progname: Some("dfdu".into()),
        progver: Some(env!("CARGO_PKG_VERSION").into()),
        timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs()),
    }
}

/// `dfdu disks`: one line per filesystem, like `df -h`.
fn disks() -> ExitCode {
    let list = match dfdu_sys::disks(dfdu_tui::DISK_TIMEOUT) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("dfdu: cannot list disks: {e}");
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
                    dfdu_tui::human(s.total),
                    dfdu_tui::human(s.used),
                    dfdu_tui::human(s.available),
                    s.used as f64 * 100.0 / s.total.max(1) as f64
                );
            }
            None => line += "  not responding",
        }
        if d.class == dfdu_sys::FsClass::Network {
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
        auto_abandon: Some(Duration::from_secs_f64(args.stall_abandon)),
        ..Default::default()
    };
    opts.limits.stall_ns = Duration::from_secs_f64(args.stall_after).as_nanos() as u64;
    let storage = dfdu_sys::storage(&args.path);
    opts.workers = args
        .threads
        .unwrap_or_else(|| dfdu_engine::workers_for(storage));
    if let Some(n) = args.measure_workers {
        opts.measure_workers = n;
    }
    if args.debug_stats {
        eprintln!("storage: {storage:?}, {} workers", opts.workers);
    }
    match args.backend {
        BackendArg::Native if inject::requested() => scan_with(
            Engine::start(
                inject::PanicOnSecondOpen::new(native_backend(storage)),
                opts,
            ),
            args,
        ),
        BackendArg::Native => scan_with(Engine::start(native_backend(storage), opts), args),
        BackendArg::Portable => scan_with(Engine::start(PortableBackend::default(), opts), args),
    }
}

fn scan_with<B: Backend>(engine: Engine<B>, args: ScanArgs) -> ExitCode {
    let ScanArgs {
        path,
        format,
        depth,
        top,
        apparent,
        like_du,
        mounts,
        rules,
        bytes,
        types,
        by_type,
        errors: list_errors,
        no_history,
        debug_stats,
        measure_workers,
        ..
    } = args;
    if (types.is_some() || by_type) && format != Format::Table && !bytes {
        eprintln!("dfdu: --type and --by-type work with the table output and --bytes only");
        return ExitCode::from(2);
    }
    let t0 = Instant::now();
    let started = report::now_secs();
    let mut opts = mounts.job();
    let measured = measure_workers.unwrap_or(EngineOptions::default().measure_workers) > 0;
    let key = history::options_key(&opts, measured);
    opts.class = rules.load().map(|r| ClassOpts::for_root(r, &path));
    engine.send(Command::Scan { path, opts });
    engine.wait_idle(None);
    if let Some(msg) = engine.read(|m| m.status.fatal.clone()) {
        eprintln!("dfdu: {msg}");
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
        let total = m.tree.total(root);
        let filter = types.as_ref();
        if bytes {
            outln!("{}", mode.of(&shown_total(&m.tree, root, filter)));
        } else if format != Format::Table {
            let out = json::ScanOut {
                mode,
                depth,
                top,
                elapsed_ms: elapsed.as_millis() as u64,
                note: &|ix| notes(m, ix),
            };
            let w = &mut std::io::BufWriter::new(std::io::stdout().lock());
            let r = match format {
                Format::Json => json::scan(w, m, root, &out),
                Format::Ndjson => json::scan_ndjson(w, m, root, &out),
                _ => dfdu_store::ncdu::export(&m.tree, root, &mut *w, &ncdu_meta()).map(|_| ()),
            };
            if let Err(e) = r.and_then(|()| std::io::Write::flush(w)) {
                if e.kind() == std::io::ErrorKind::BrokenPipe {
                    return ExitCode::SUCCESS;
                }
                eprintln!("dfdu: writing the output: {e}");
                return ExitCode::from(1);
            }
            if list_errors {
                print_errors(m, root);
            }
        } else {
            print_tree(m, root, depth.unwrap_or(1), top.unwrap_or(10), mode, filter);
            if by_type {
                print_by_type(&m.tree, root, mode, filter);
            }
            if list_errors {
                print_errors(m, root);
            }
            print_footer(m, root, elapsed, list_errors);
        }
        if debug_stats {
            print_debug_stats(m);
        }
        if !no_history {
            history::record(m, root, &key, started);
        }
        if total.errors > 0 || m.status.incomplete {
            ExitCode::from(EXIT_PARTIAL)
        } else {
            ExitCode::SUCCESS
        }
    });
    if debug_stats {
        for g in engine.scanner().gate_stats() {
            eprintln!(
                "mount limit: mount {} (dev {}): {} at once, at most {} were; {} tasks waited, \
                 {} permits taken back from stuck calls",
                g.mount, g.dev, g.limit, g.peak, g.deferred, g.revoked
            );
        }
    }
    engine.shutdown(Duration::from_millis(500));
    code
}

fn size(tree: &Tree, ix: Ix, mode: SizeMode) -> u64 {
    mode.of(&tree.total(ix))
}

/// The totals shown for `ix`: everything, or with a filter only the matching files below it.
fn shown_total(tree: &Tree, ix: Ix, filter: Option<&TypeFilter>) -> dfdu_tree::Totals {
    match filter {
        Some(f) => filetype::sum(tree, ix, f, None),
        None => tree.total(ix),
    }
}

/// The tree `depth` levels deep, `top` entries per directory. With a type filter, sizes count
/// the matching files only and entries without any are left out.
fn print_tree(
    m: &Model,
    root: Ix,
    depth: usize,
    top: usize,
    mode: SizeMode,
    filter: Option<&TypeFilter>,
) {
    let tree = &m.tree;
    let root_total = shown_total(tree, root, filter);
    if let Some(f) = filter {
        outln!("only {f}: {} files", root_total.items);
    }
    outln!(
        "{:>10}  {}{}",
        dfdu_tui::human(mode.of(&root_total)),
        String::from_utf8_lossy(tree.name(root)),
        note(m, root)
    );
    // Depth first, largest first, `top` entries per directory.
    let mut stack = vec![(root, 0usize, 0u64)];
    while let Some((ix, d, bytes)) = stack.pop() {
        if ix != root {
            let slash = if tree.dir(ix).is_some() { "/" } else { "" };
            outln!(
                "{:>10}  {}{}{}{}",
                dfdu_tui::human(bytes),
                "  ".repeat(d),
                String::from_utf8_lossy(tree.name(ix)),
                slash,
                note(m, ix)
            );
        }
        if d >= depth {
            continue;
        }
        // Data the entries share is counted at the directory.
        if mode == SizeMode::Disk && filter.is_none() && tree.home(ix) > 0 {
            outln!(
                "{:>10}  {}(shared between entries)",
                dfdu_tui::human(tree.home(ix)),
                "  ".repeat(d + 1)
            );
        }
        let mut kids: Vec<(Ix, u64)> = tree
            .children(ix)
            .filter_map(|c| match filter {
                Some(f) => {
                    let t = filetype::sum(tree, c, f, None);
                    (t.items > 0).then(|| (c, mode.of(&t)))
                }
                None => Some((c, size(tree, c, mode))),
            })
            .collect();
        kids.sort_by_key(|&(_, b)| std::cmp::Reverse(b));
        for &(c, b) in kids.iter().take(top).rev() {
            stack.push((c, d + 1, b));
        }
    }
}

/// Disk usage by file type, then the largest extensions, of the files below `root` (those
/// `filter` matches, with one).
fn print_by_type(tree: &Tree, root: Ix, mode: SizeMode, filter: Option<&TypeFilter>) {
    let mut b = filetype::Breakdown::default();
    let all = TypeFilter::default();
    filetype::sum(tree, root, filter.unwrap_or(&all), Some(&mut b));
    let files = |n: u64| {
        if n == 1 {
            "1 file".into()
        } else {
            format!("{n} files")
        }
    };
    outln!();
    outln!("By type:");
    let mut types: Vec<_> = filetype::FileType::ALL
        .iter()
        .map(|&t| (t, b.by_type[t as usize]))
        .filter(|(_, t)| t.items > 0)
        .collect();
    types.sort_by_key(|(_, t)| std::cmp::Reverse(mode.of(t)));
    for (ty, t) in types {
        outln!(
            "{:>10}  {:<12}{:>14}",
            dfdu_tui::human(mode.of(&t)),
            ty.name(),
            files(t.items)
        );
    }
    let exts = b.exts_by(|t| mode.of(t));
    if !exts.is_empty() {
        outln!();
        outln!("Largest extensions:");
        for (e, t) in exts.iter().take(15) {
            outln!(
                "{:>10}  {:<12}{:>14}  {}",
                dfdu_tui::human(mode.of(t)),
                format!(".{e}"),
                files(t.items),
                e.file_type()
            );
        }
    }
}

fn type_filter(s: &str) -> Result<TypeFilter, String> {
    let f = TypeFilter::parse(s)?;
    if f.is_empty() {
        return Err("name at least one type or extension".into());
    }
    Ok(f)
}

/// [`notes`] in brackets after a name, or nothing.
fn note(m: &Model, ix: Ix) -> String {
    let n = notes(m, ix);
    if n.is_empty() { n } else { format!("  [{n}]") }
}

/// Why an entry looks the way it does: not scanned, a second mount, a btrfs subvolume.
fn notes(m: &Model, ix: Ix) -> String {
    let tree = &m.tree;
    let mut parts = vec![];
    if let Some(p) = dfdu_tui::problem(tree, ix) {
        parts.push(p);
    }
    match dfdu_tui::errors_below(tree, ix) {
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
    if let Some(l) = dfdu_tui::subvol_label(m, ix) {
        parts.push(l);
    }
    if tree.flags(ix).has(NodeFlags::COMPRESSED) {
        parts.push("compressed: at most this".into());
    }
    let out = tree.total(ix).shared_out;
    if out > 0 {
        parts.push(format!(
            "{} also used outside the scan",
            dfdu_tui::human(out)
        ));
    }
    parts.join("; ")
}

/// Every entry that could not be read, in path order.
fn print_errors(m: &Model, root: Ix) {
    let list = dfdu_tui::errors(&m.tree, root, usize::MAX);
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
        let list = dfdu_tui::errors(&m.tree, root, 0);
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
            dfdu_tui::human(t.referenced - t.alloc),
            dfdu_tui::human(t.referenced)
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

/// `--debug-stats`: what went through the pipeline and how the applier spent its time (entries
/// applied per second of lock time, hold and wait times per slice).
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
    let (kids, kids_freed) = m.tree.child_slots();
    eprintln!(
        "tree: {} nodes, {:.1} MiB of names ({:.1} MiB freed), {:.1} MiB of child lists \
         ({:.1} MiB freed)",
        m.tree.live_nodes(),
        names as f64 / 1048576.0,
        garbage as f64 / 1048576.0,
        kids as f64 * 4.0 / 1048576.0,
        kids_freed as f64 * 4.0 / 1048576.0
    );
    let locked = Duration::from_nanos(t.locked_ns);
    let rate = s.entries as f64 / locked.as_secs_f64().max(1e-9);
    let long = Timing::at_least(&t.hold, Duration::from_millis(2));
    eprintln!(
        "applier: {:.0} ms holding the write lock in {} slices, {:.2}M entries/s; hold p50 < {:?}, \
         p99 < {:?}, max < {:?}, {long} slices ({:.2}%) over 2 ms; wait for the lock p99 < {:?}, \
         max < {:?}",
        locked.as_secs_f64() * 1000.0,
        t.slices,
        rate / 1e6,
        Timing::percentile(&t.hold, 0.5),
        Timing::percentile(&t.hold, 0.99),
        Timing::max(&t.hold),
        long as f64 * 100.0 / t.slices.max(1) as f64,
        Timing::percentile(&t.wait, 0.99),
        Timing::max(&t.wait),
    );
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn theme_and_color_flags_parse() {
        let cli = Cli::try_parse_from(["dfdu", "/x"]).expect("defaults");
        assert_eq!(cli.theme, "auto");
        assert_eq!(cli.color, ColorFlag(None));
        assert!(!cli.no_mouse && !cli.debug_stats);

        let cli = Cli::try_parse_from([
            "dfdu",
            "--theme",
            "light",
            "--color",
            "256",
            "--no-mouse",
            "--debug-stats",
            "/x",
        ])
        .expect("flags");
        assert_eq!(cli.theme, "light");
        assert_eq!(cli.color, ColorFlag(Some(ColorLevel::Ansi256)));
        assert!(cli.no_mouse && cli.debug_stats);
        assert_eq!(cli.path, Some(PathBuf::from("/x")));

        for (value, level) in [
            ("never", ColorLevel::None),
            ("16", ColorLevel::Ansi16),
            ("truecolor", ColorLevel::TrueColor),
        ] {
            let cli = Cli::try_parse_from(["dfdu", "--color", value]).expect(value);
            assert_eq!(cli.color, ColorFlag(Some(level)));
        }
        assert!(Cli::try_parse_from(["dfdu", "--color", "always"]).is_err());
        // Browser options are not options of `dfdu scan`.
        assert!(Cli::try_parse_from(["dfdu", "--theme", "dark", "scan", "/x"]).is_err());
    }

    #[test]
    fn bad_theme_file_is_reported() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let themes = tmp.path();
        let write = |name: &str, text: &str| fs::write(themes.join(name), text).expect("write");
        write("broken.toml", "base = \"sepia\"\n");
        write("typo.toml", "[tokens]\nfg = \"#12345\"\n");
        write("syntax.toml", "base = \n");
        write(
            "nord.toml",
            "base = \"light\"\n[tokens]\nfg = \"#2e3440\"\n",
        );
        fs::create_dir(themes.join("dir.toml")).expect("mkdir");

        let err = |name: &str| theme_choice(name, Some(themes)).expect_err(name);
        let missing = err("missing");
        assert!(missing.contains("not a built-in theme"), "{missing}");
        assert!(missing.contains("missing.toml"), "{missing}");
        let broken = err("broken");
        assert!(
            broken.contains("broken.toml") && broken.contains("sepia"),
            "{broken}"
        );
        let typo = err("typo");
        assert!(typo.contains("`fg`") && typo.contains("#12345"), "{typo}");
        assert!(err("syntax").contains("syntax.toml"));
        assert!(err("dir").contains("dir.toml"));
        assert!(err("../nord").contains("not a built-in theme"));
        assert!(theme_choice("nord", None).is_err());

        match theme_choice("nord", Some(themes)) {
            Ok(ThemeChoice::File(file)) => {
                assert_eq!(file.name.as_deref(), Some("nord"));
                assert_eq!(file.base(), "light");
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            theme_choice("high-contrast", None),
            Ok(ThemeChoice::Builtin("high-contrast"))
        ));
        assert!(matches!(theme_choice("auto", None), Ok(ThemeChoice::Auto)));
    }
}

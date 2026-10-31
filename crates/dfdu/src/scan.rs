//! `dfdu scan`: scans a directory and prints a summary (a table, JSON, NDJSON or an ncdu
//! export).

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::Args;
use dfdu_engine::{Command, Engine, EngineOptions, SizeMode};
use dfdu_scan::{Backend, JobOptions, PortableBackend};
use dfdu_tree::HostPath;
use dfdu_tree::filetype::TypeFilter;

use crate::launch::{self, Local};
use crate::table::{
    notes, print_by_type, print_debug_stats, print_errors, print_footer, print_tree, shown_total,
};
use crate::{
    EXIT_PARTIAL, MountArgs, RuleArgs, history, inject, json, outside, remote, transfer, written,
};

#[derive(Args)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "command-line flags, each an independent switch"
)]
pub struct ScanArgs {
    #[arg(default_value = ".")]
    pub path: PathBuf,
    /// Output: table (for people), json (one document, schema/scan.v1.json), ndjson (one
    /// record per line, for large trees) or ncdu (ncdu's JSON export).
    #[arg(long, value_enum, default_value_t = Format::Table)]
    pub format: Format,
    /// Levels of directories to print below the root [default: 1; every level for ndjson].
    #[arg(long)]
    pub depth: Option<usize>,
    /// Entries to print per directory, largest first [default: 10; all for ndjson].
    #[arg(long)]
    pub top: Option<usize>,
    /// Show apparent sizes instead of disk usage.
    #[arg(long, conflicts_with = "like_du")]
    pub apparent: bool,
    /// Count data shared between files (btrfs snapshots, reflinks, APFS clones) once per file,
    /// as du does, instead of once.
    #[arg(long)]
    pub like_du: bool,
    /// Worker threads for the scanned filesystem. Mounts below it with their own limit (spinning
    /// disks: 2, network: 4) add that many.
    #[arg(long)]
    pub threads: Option<usize>,
    /// Threads measuring shared extents (btrfs, XFS; APFS clones of 1 MiB or more); 0 reports
    /// sizes as du does.
    #[arg(long)]
    pub measure_workers: Option<usize>,
    /// Scan PATH on this host, through ssh; the dfdu agent is sent there if it has none
    /// (--remote-agent). Nothing is recorded in the history.
    #[arg(long, value_name = "HOST")]
    pub remote: Option<String>,
    #[arg(
        long,
        value_name = "HOW",
        value_enum,
        requires = "remote",
        help = crate::remote::AGENT_MODE_HELP
    )]
    pub remote_agent: Option<crate::remote::AgentMode>,
    #[command(flatten)]
    pub mounts: MountArgs,
    #[command(flatten)]
    pub rules: RuleArgs,
    /// Count only files of these types or extensions, comma-separated: `video,audio,.iso`.
    /// Types: video, audio, image, document, archive, disk-image, code, executable, font, data,
    /// other. Folders show the size of the matching files inside them.
    #[arg(long = "type", value_name = "TYPES", value_parser = crate::type_filter)]
    pub types: Option<TypeFilter>,
    /// After the tree, print disk usage by file type and the largest extensions.
    #[arg(long)]
    pub by_type: bool,
    /// Print only the root total in bytes (for scripts and tests).
    #[arg(long)]
    pub bytes: bool,
    /// List every entry that could not be read, with the reason.
    #[arg(long)]
    pub errors: bool,
    /// Do not record the scan in the history (`dfdu diff`, `dfdu history`).
    #[arg(long)]
    pub no_history: bool,
    /// Abandon directories stalled this many seconds.
    #[arg(long, default_value_t = 30.0, value_parser = crate::seconds)]
    pub stall_abandon: f64,
    /// Report a directory as stalled after one call on it took this many seconds (tests).
    #[arg(long, default_value_t = 10.0, value_parser = crate::seconds, hide = true)]
    pub stall_after: f64,
    /// Print pipeline counters and applier timing (lock hold and wait) after the scan.
    #[arg(long, hide = true)]
    pub debug_stats: bool,
    /// Filesystem access: `native` (the fastest for this system) or `portable` (std::fs, the
    /// reference the native backends are tested against).
    #[expect(
        clippy::doc_markdown,
        reason = "clap prints this as the help text, where backticks would show"
    )]
    #[arg(long, value_enum, default_value_t = BackendArg::Native, hide = true)]
    pub backend: BackendArg,
}

#[derive(clap::ValueEnum, Clone, Copy, PartialEq)]
pub enum Format {
    Table,
    Json,
    Ndjson,
    Ncdu,
}

#[derive(clap::ValueEnum, Clone, Copy)]
pub enum BackendArg {
    Native,
    Portable,
}

impl ScanArgs {
    /// What `dfdu scan` gets without arguments.
    #[expect(
        clippy::expect_used,
        reason = "every argument of `dfdu scan` has a default, so no arguments parse"
    )]
    pub fn defaults() -> ScanArgs {
        use clap::FromArgMatches;
        let matches = ScanArgs::augment_args(clap::Command::new("scan")).get_matches_from(["scan"]);
        ScanArgs::from_arg_matches(&matches).expect("the defaults parse")
    }
}

pub fn run(args: ScanArgs) -> ExitCode {
    if (args.types.is_some() || args.by_type) && args.format != Format::Table && !args.bytes {
        eprintln!("dfdu: --type and --by-type work with the table output and --bytes only");
        return ExitCode::from(2);
    }
    let mut opts = EngineOptions {
        auto_abandon: Some(Duration::from_secs_f64(args.stall_abandon)),
        ..EngineOptions::from_env()
    };
    opts.limits.stall_ns =
        u64::try_from(Duration::from_secs_f64(args.stall_after).as_nanos()).unwrap_or(u64::MAX);
    if let Some(n) = args.measure_workers {
        opts.measure_workers = n;
    }
    let base = args.mounts.job();
    let key = dfdu_store::history::options_key(&base, opts.measure_workers > 0);
    // The agent scans with its own workers; the path is on its host, not this one.
    if let Some(host) = args.remote.as_deref() {
        let rules = args.rules.remote();
        return match remote::engine(host, opts, base, &args.path, rules, args.remote_agent) {
            Ok((e, job, root)) => scan_with(e, job, root, &key, args),
            Err(code) => code,
        };
    }
    let local = Local {
        mft: args.mounts.use_mft(&args.path),
        threads: args.threads,
        job: base,
        rules: args.rules.load(),
    };
    let (storage, opts) = local.engine_options(&args.path, opts);
    if args.debug_stats {
        eprintln!("storage: {storage:?}, {} workers", opts.workers);
    }
    let job = local.job(&args.path);
    let root = HostPath::local(&args.path);
    match args.backend {
        BackendArg::Native if inject::requested() => scan_with(
            Engine::start(inject::PanicOnSecondOpen::new(local.backend(storage)), opts),
            job,
            root,
            &key,
            args,
        ),
        BackendArg::Native => scan_with(
            Engine::start(local.backend(storage), opts),
            job,
            root,
            &key,
            args,
        ),
        BackendArg::Portable => scan_with(
            Engine::start(PortableBackend::default(), opts),
            job,
            root,
            &key,
            args,
        ),
    }
}

/// Runs the scan of `root` (`args.path`, or where the agent resolved it) with `engine` and
/// `job`, prints it, and records it in the history under `key`.
fn scan_with<B: Backend>(
    engine: Engine<B>,
    job: JobOptions,
    root: HostPath,
    key: &str,
    args: ScanArgs,
) -> ExitCode {
    let ScanArgs {
        format,
        depth,
        top,
        apparent,
        like_du,
        bytes,
        types,
        by_type,
        errors: list_errors,
        no_history,
        debug_stats,
        remote,
        ..
    } = args;
    // The history is about this host's files.
    let no_history = no_history || remote.is_some();
    let t0 = Instant::now();
    let started = dfdu_tree::now_secs();
    let engine = match launch::run_scan(
        engine,
        Command::Scan {
            path: root,
            opts: job,
        },
    ) {
        Ok(e) => e,
        Err(code) => return code,
    };
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
            let left_out = match format {
                Format::Json | Format::Ndjson => Some(outside::gather(m, root, remote.is_none())),
                _ => None,
            };
            let out = json::ScanOut {
                mode,
                depth,
                top,
                elapsed_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
                note: &|ix| notes(m, ix),
                outside: left_out.as_ref(),
            };
            let w = &mut std::io::BufWriter::new(std::io::stdout().lock());
            let r = match format {
                Format::Json => json::scan(w, m, root, &out),
                Format::Ndjson => json::scan_ndjson(w, m, root, &out),
                _ => dfdu_store::ncdu::export(&m.tree, root, &mut *w, &transfer::ncdu_meta())
                    .map(|_| ()),
            };
            if let Err(code) = written(r.and_then(|()| std::io::Write::flush(w))) {
                return code;
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
            outside::footer(m, root, remote.is_none());
        }
        if debug_stats {
            print_debug_stats(m);
        }
        if !no_history {
            history::record(m, root, key, started);
        }
        if total.errors > 0 || m.status.incomplete {
            ExitCode::from(EXIT_PARTIAL)
        } else {
            ExitCode::SUCCESS
        }
    });
    if debug_stats {
        let gates = engine.local_scanner().map(|s| s.gate_stats());
        for g in gates.unwrap_or_default() {
            eprintln!(
                "mount limit: mount {} (dev {}): {} at once, at most {} were; {} tasks waited, \
                 {} permits taken back from stuck calls",
                g.mount, g.dev, g.limit, g.peak, g.deferred, g.revoked
            );
        }
    }
    engine.shutdown(launch::SHUTDOWN);
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_the_clap_defaults() {
        let a = ScanArgs::defaults();
        assert_eq!(a.path, PathBuf::from("."));
        assert!(a.format == Format::Table && matches!(a.backend, BackendArg::Native));
        assert_eq!((a.stall_abandon, a.stall_after), (30.0, 10.0));
        assert_eq!(
            Duration::from_secs_f64(a.stall_abandon),
            launch::STALL_ABANDON
        );
        assert!(a.depth.is_none() && a.top.is_none() && a.threads.is_none());
        assert!(!a.bytes && !a.errors && !a.no_history && !a.debug_stats && !a.mounts.mft);
    }
}

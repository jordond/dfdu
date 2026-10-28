//! `dfdu export` and `dfdu import`: scans written to and read from files, in ncdu's JSON
//! format or dfdu's own snapshot format.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Args;
use dfdu_engine::{Command, Model};
use dfdu_tree::filetype::TypeFilter;
use dfdu_tui::SizeMode;

use crate::launch::{self, Local};
use crate::table::{print_by_type, print_tree, shown_total};
use crate::{EXIT_PARTIAL, MountArgs, remote, written_to};

#[derive(Args)]
pub struct ExportArgs {
    #[arg(default_value = ".")]
    pub path: PathBuf,
    /// Where to write (`-` for stdout).
    #[arg(short, long, value_name = "FILE", default_value = "-")]
    pub output: PathBuf,
    /// Compress with zstd (ncdu reads it with `-f`; dfdu recognises it on import).
    #[arg(long, conflicts_with = "snapshot")]
    pub compress: bool,
    /// Write dfdu's own snapshot format instead (lossless, checked, compressed; for
    /// `dfdu -f` and `--resume-from`, not for ncdu).
    #[arg(long)]
    pub snapshot: bool,
    /// Worker threads.
    #[arg(long)]
    pub threads: Option<usize>,
    /// Start from this snapshot (`--snapshot`) and rescan only the directories changed since
    /// it was taken, when the volume's change journal can tell (macOS, Windows, btrfs as root);
    /// otherwise everything.
    /// PATH and the mount options are the snapshot's.
    #[arg(long, value_name = "SNAPSHOT", conflicts_with = "remote")]
    pub resume_from: Option<PathBuf>,
    /// Scan PATH on this host, through `ssh HOST dfdu agent` (dfdu must be installed there).
    #[arg(long, value_name = "HOST")]
    pub remote: Option<String>,
    #[command(flatten)]
    pub mounts: MountArgs,
}

#[derive(Args)]
pub struct ImportArgs {
    /// The export (`-` for stdin).
    pub file: PathBuf,
    /// Levels of directories to print below the root.
    #[arg(long, default_value_t = 1)]
    pub depth: usize,
    /// Entries to print per directory, largest first.
    #[arg(long, default_value_t = 10)]
    pub top: usize,
    /// Show apparent sizes instead of disk usage.
    #[arg(long)]
    pub apparent: bool,
    /// Count only files of these types or extensions, comma-separated: `video,audio,.iso`.
    /// Types: video, audio, image, document, archive, disk-image, code, executable, font, data,
    /// other. Folders show the size of the matching files inside them.
    #[arg(long = "type", value_name = "TYPES", value_parser = crate::type_filter)]
    pub types: Option<TypeFilter>,
    /// After the tree, print disk usage by file type and the largest extensions.
    #[arg(long)]
    pub by_type: bool,
    /// Print only the root total in bytes.
    #[arg(long)]
    pub bytes: bool,
}

/// Where the change journal of `path`'s volume stands (FSEvents on macOS, the USN journal on
/// Windows, the btrfs subvolume's generation on Linux as root); `None` where there is none.
fn journal_position(path: &Path) -> Option<dfdu_store::snapshot::Journal> {
    // Read before the position, so that whatever changes after it has a ctime no older.
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (volume, event_id) = dfdu_engine::journal_position(path)?;
    Some(dfdu_store::snapshot::Journal {
        volume,
        event_id,
        time,
    })
}

/// Reads a snapshot or an ncdu export from `file` (`-` for stdin), reporting a failure.
pub fn read_export(file: &Path) -> Result<dfdu_store::Opened, ExitCode> {
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

/// `dfdu import`: the summary `dfdu scan` prints, for a scan read from a file.
pub fn import(args: ImportArgs) -> ExitCode {
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
pub fn export(args: ExportArgs) -> ExitCode {
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
    let opts = launch::headless(false);
    let (engine, job) = match &args.remote {
        Some(host) => {
            let (_, opts) = launch::for_storage(&path, args.threads, opts);
            match remote::engine(host, opts, None) {
                Ok((e, _)) => (e, job),
                Err(code) => return code,
            }
        }
        None => {
            let mut local = Local::new(&path, args.mounts, args.threads, None);
            local.job = job;
            local.start(&path, opts)
        }
    };
    // Taken before the scan, so that what changes during it is replayed on resume.
    let journal = journal_position(&path).filter(|_| args.remote.is_none());
    let cmd = match resumed {
        Some(l) => {
            let changed = l
                .header
                .journal
                .and_then(|j| dfdu_engine::changed_since(&path, j.volume, j.event_id, j.time));
            let node = l.tree.node_ref(l.root);
            engine.load(l.tree);
            Command::Resume {
                node,
                opts: job.clone(),
                changed,
            }
        }
        None => Command::Scan {
            path: path.clone(),
            opts: job.clone(),
        },
    };
    let engine = match launch::run_scan(engine, cmd) {
        Ok(e) => e,
        Err(code) => return code,
    };
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
                let mut z = dfdu_store::ZstdWriter::new(w);
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
    engine.shutdown(launch::SHUTDOWN);
    let (stats, partial) = match written_to(&args.output.display(), result) {
        Ok(r) => r,
        Err(code) => return code,
    };
    if stats.non_utf8 > 0 {
        eprintln!(
            "dfdu: {} names are not UTF-8 and were written as they are; ncdu reads them, most \
             JSON tools do not",
            stats.non_utf8
        );
    }
    if partial {
        ExitCode::from(EXIT_PARTIAL)
    } else {
        ExitCode::SUCCESS
    }
}

/// What an ncdu export says about its writer.
pub fn ncdu_meta() -> dfdu_store::ncdu::Metadata {
    dfdu_store::ncdu::Metadata {
        progname: Some("dfdu".into()),
        progver: Some(env!("CARGO_PKG_VERSION").into()),
        timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs()),
    }
}

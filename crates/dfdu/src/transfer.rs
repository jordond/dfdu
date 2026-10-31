//! `dfdu export` and `dfdu import`: scans written to and read from files, in ncdu's JSON
//! format or dfdu's own snapshot format.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Args;
use dfdu_engine::{Command, Model, SizeMode};
use dfdu_tree::HostPath;
use dfdu_tree::filetype::TypeFilter;

use crate::launch::{self, Local};
use crate::table::{print_by_type, print_tree, shown_total};
use crate::{EXIT_PARTIAL, MountArgs, print_out, remote, written_to};

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
    /// Scan PATH on this host, through ssh; the dfdu agent is sent there if it has none
    /// (--remote-agent).
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

/// Reads a snapshot or an ncdu export from `file` (`-` for stdin), reporting a failure.
pub fn read_export(file: &Path) -> Result<dfdu_store::Opened, ExitCode> {
    let limits = dfdu_store::Limits::default();
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
pub fn import(args: &ImportArgs) -> Result<ExitCode, ExitCode> {
    let im = read_export(&args.file)?;
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
    print_out(|w| {
        if args.bytes {
            return writeln!(w, "{}", mode.of(&shown_total(&m.tree, im.root, filter)));
        }
        print_tree(w, &m, im.root, args.depth, args.top, mode, filter)?;
        if args.by_type {
            print_by_type(w, &m.tree, im.root, mode, filter)?;
        }
        let by = (im.written_by.as_ref())
            .map(|by| format!(", written by {by}"))
            .unwrap_or_default();
        writeln!(w, "{} entries{by}", im.entries)
    })?;
    Ok(if total.errors > 0 {
        ExitCode::from(EXIT_PARTIAL)
    } else {
        ExitCode::SUCCESS
    })
}

/// The snapshot in `f`, read; reports a failure.
fn read_snapshot(f: &Path) -> Result<dfdu_store::snapshot::Loaded, ExitCode> {
    std::fs::File::open(f)
        .map_err(dfdu_store::ImportError::from)
        .and_then(|r| {
            dfdu_store::snapshot::read(std::io::BufReader::new(r), &dfdu_store::Limits::default())
        })
        .map_err(|e| {
            eprintln!("dfdu: {}: {e}", f.display());
            ExitCode::from(1)
        })
}

/// `dfdu export`: scans `path` and writes it in ncdu's format.
pub fn export(args: &ExportArgs) -> Result<ExitCode, ExitCode> {
    let resumed = args.resume_from.as_deref().map(read_snapshot).transpose()?;
    let job = args
        .mounts
        .job_resuming(resumed.as_ref().map(|l| &l.header));
    let path = match &resumed {
        Some(l) => l.tree.path_buf(l.root),
        None => args.path.clone(),
    };
    let opts = launch::headless(false);
    // A remote scan's root is the one the agent resolved on its host.
    let (engine, job, root) = if let Some(host) = &args.remote {
        remote::engine(host, opts, job, &path, None, args.remote_agent)?
    } else {
        let local = Local {
            mft: args.mounts.use_mft(&path),
            threads: args.threads,
            job,
            rules: None,
        };
        let (engine, job) = local.start(&path, opts);
        (engine, job, HostPath::local(&path))
    };
    // Taken before the scan, so that what changes during it is replayed on resume.
    let journal = if args.remote.is_none() {
        dfdu_engine::journal_position(&path)
    } else {
        None
    };
    let started = (
        std::time::Instant::now(),
        dfdu_tree::now_secs().max(0).cast_unsigned(),
    );
    // A resume from the journal keeps the last full scan's time; anything else is one.
    let full_scan = match resumed {
        Some(l) => {
            let kept = l
                .header
                .journal
                .is_some()
                .then(|| (l.header.full_scan_at(), l.header.scan_ms));
            engine.resume(l.tree, l.header.journal, job.clone());
            kept
        }
        None => {
            engine.send(Command::Scan {
                path: root,
                opts: job.clone(),
            });
            None
        }
    };
    let engine = launch::wait_scan(engine)?;
    let (last_full_scan, scan_ms) = full_scan.unwrap_or_else(|| {
        let ms = u64::try_from(started.0.elapsed().as_millis()).unwrap_or(u64::MAX);
        (started.1, ms)
    });
    let meta = ncdu_meta();
    let to_stdout = args.output == Path::new("-");
    let result = engine.read(|m| {
        let root = m
            .tree
            .roots()
            .next()
            .ok_or_else(|| std::io::Error::other("no scan"))?;
        let partial = m.tree.total(root).errors > 0 || m.status.incomplete;
        let write = |w: &mut dyn std::io::Write| {
            let header = args.snapshot.then(|| dfdu_store::snapshot::Header {
                tool_version: env!("CARGO_PKG_VERSION").into(),
                written: meta.timestamp.unwrap_or(0),
                cross_mounts: job.cross_mounts,
                network: job.network,
                journal,
                last_full_scan,
                scan_ms,
            });
            write_export(w, &m.tree, root, header.as_ref(), args.compress, &meta)
        };
        let stats = if to_stdout {
            write(&mut std::io::stdout().lock())?
        } else {
            dfdu_store::write_atomic(&args.output, |f| write(f))?
        };
        Ok::<_, std::io::Error>((stats, partial))
    });
    engine.shutdown(launch::SHUTDOWN);
    let (stats, partial) = written_to(&args.output.display(), result)?;
    if stats.non_utf8 > 0 {
        eprintln!(
            "dfdu: {} names are not UTF-8 and were written as they are; ncdu reads them, most \
             JSON tools do not",
            stats.non_utf8
        );
    }
    Ok(if partial {
        ExitCode::from(EXIT_PARTIAL)
    } else {
        ExitCode::SUCCESS
    })
}

/// Writes the scan below `root` to `w`: as a snapshot with `header` when there is one, else in
/// ncdu's format, zstd-compressed when `compress` is set.
fn write_export(
    w: &mut dyn std::io::Write,
    tree: &dfdu_tree::Tree,
    root: dfdu_tree::Ix,
    header: Option<&dfdu_store::snapshot::Header>,
    compress: bool,
    meta: &dfdu_store::ncdu::Metadata,
) -> std::io::Result<dfdu_store::ncdu::ExportStats> {
    if let Some(header) = header {
        let entries = dfdu_store::snapshot::write(tree, root, w, header)?;
        Ok(dfdu_store::ncdu::ExportStats {
            entries,
            non_utf8: 0,
        })
    } else if compress {
        let mut z = dfdu_store::ZstdWriter::new(w);
        let s = dfdu_store::ncdu::export(tree, root, &mut z, meta)?;
        z.finish()?;
        Ok(s)
    } else {
        dfdu_store::ncdu::export(tree, root, w, meta)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// What `write_export` writes for a one-directory tree: a snapshot with a header, ncdu's
    /// JSON compressed or plain otherwise.
    #[test]
    fn export_format_follows_the_options() {
        let tmp = tempfile::tempdir().unwrap();
        let meta = dfdu_tree::Meta::from_metadata(&std::fs::metadata(tmp.path()).unwrap());
        let mut tree = dfdu_tree::Tree::new();
        let root = tree.new_root(tmp.path().as_os_str().as_encoded_bytes(), &meta, true);
        let header = dfdu_store::snapshot::Header {
            tool_version: "test".into(),
            written: 0,
            cross_mounts: false,
            network: false,
            journal: None,
            last_full_scan: 0,
            scan_ms: 0,
        };
        let write = |header, compress| {
            let mut out = Vec::new();
            write_export(&mut out, &tree, root, header, compress, &ncdu_meta()).unwrap();
            out
        };
        assert!(write(Some(&header), true).starts_with(b"DFDUSNAP"));
        assert!(write(None, true).starts_with(&[0x28, 0xb5, 0x2f, 0xfd]));
        assert!(write(None, false).starts_with(b"[1,2,{\"progname\":\"dfdu\""));
    }

    /// A snapshot read back for `--resume-from` is rescanned with its own mount options, an
    /// ncdu export with those given.
    #[test]
    fn a_resumed_snapshot_keeps_its_mount_options() {
        let tmp = tempfile::tempdir().unwrap();
        let meta = dfdu_tree::Meta::from_metadata(&std::fs::metadata(tmp.path()).unwrap());
        let mut tree = dfdu_tree::Tree::new();
        let root = tree.new_root(tmp.path().as_os_str().as_encoded_bytes(), &meta, true);
        let header = dfdu_store::snapshot::Header {
            cross_mounts: true,
            network: true,
            ..Default::default()
        };
        let reread = |header: Option<&dfdu_store::snapshot::Header>, name: &str| {
            let file = tmp.path().join(name);
            let mut out = Vec::new();
            write_export(&mut out, &tree, root, header, false, &ncdu_meta()).unwrap();
            std::fs::write(&file, out).unwrap();
            read_export(&file).unwrap_or_else(|_| panic!("{name} reads back"))
        };
        let given = MountArgs {
            one_file_system: true,
            ..MountArgs::default()
        };
        let snap = reread(Some(&header), "scan.dfdu");
        let job = given.job_resuming(snap.header.as_ref());
        assert!(job.cross_mounts && job.network);
        let ncdu = reread(None, "scan.json");
        assert!(ncdu.header.is_none());
        let job = given.job_resuming(ncdu.header.as_ref());
        assert!(!job.cross_mounts && !job.network);
    }
}

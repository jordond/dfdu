//! `dfdu export` and `dfdu import`: scans written to and read from files, in ncdu's JSON
//! format or dfdu's own snapshot format.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Args;
use dfdu_engine::{Command, Model, SizeMode};
use dfdu_tree::filetype::TypeFilter;

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
pub fn import(args: &ImportArgs) -> ExitCode {
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

/// The snapshot in `f`, read; reports a failure.
fn read_snapshot(f: &Path) -> Result<dfdu_store::snapshot::Loaded, ExitCode> {
    std::fs::File::open(f)
        .map_err(dfdu_store::ImportError::from)
        .and_then(|r| {
            dfdu_store::snapshot::read(
                std::io::BufReader::new(r),
                &dfdu_store::ncdu::Limits::default(),
            )
        })
        .map_err(|e| {
            eprintln!("dfdu: {}: {e}", f.display());
            ExitCode::from(1)
        })
}

/// `dfdu export`: scans `path` and writes it in ncdu's format.
pub fn export(args: &ExportArgs) -> ExitCode {
    let resumed = match args.resume_from.as_deref().map(read_snapshot).transpose() {
        Ok(l) => l,
        Err(code) => return code,
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
    let (engine, job) = if let Some(host) = &args.remote {
        match remote::engine(host, opts, None) {
            Ok((e, _)) => (e, job),
            Err(code) => return code,
        }
    } else {
        let local = Local {
            mft: args.mounts.use_mft(&path),
            threads: args.threads,
            job,
            rules: None,
        };
        local.start(&path, opts)
    };
    // Taken before the scan, so that what changes during it is replayed on resume.
    let journal = if args.remote.is_none() {
        dfdu_engine::journal_position(&path)
    } else {
        None
    };
    match resumed {
        Some(l) => engine.resume(l.tree, l.header.journal, job.clone()),
        None => engine.send(Command::Scan {
            path: path.clone(),
            opts: job.clone(),
        }),
    }
    let engine = match launch::wait_scan(engine) {
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
        let write = |w: &mut dyn std::io::Write| {
            let header = args.snapshot.then(|| dfdu_store::snapshot::Header {
                tool_version: env!("CARGO_PKG_VERSION").into(),
                written: meta.timestamp.unwrap_or(0),
                cross_mounts: job.cross_mounts,
                network: job.network,
                journal,
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
}

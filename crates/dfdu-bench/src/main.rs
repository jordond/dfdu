//! `dfdu-bench gen`: a seeded synthetic tree for benchmarks. The same seed and
//! size give the same tree on every machine and with any number of threads, so timings taken on
//! different days or machines compare like with like.
//!
//! The shape is [`dfdu_bench::Plan`]'s. Files above 512 bytes are sparse (`set_len`), so a
//! 10M-entry tree takes a few GiB of disk rather than terabytes; scan cost depends on the number
//! of entries, not on their contents.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use dfdu_bench::{Entry, Plan, index};

#[derive(Parser)]
#[command(about)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a synthetic tree in DIR (which must not exist or be empty).
    Gen {
        dir: PathBuf,
        /// Entries (files, directories and links) in the tree, the root not included.
        #[arg(long, default_value_t = 1_000_000)]
        entries: u64,
        #[arg(long, default_value_t = 1)]
        seed: u64,
        /// Also put this many files in one directory, `wide/` (counted in --entries).
        #[arg(long, default_value_t = 0)]
        wide: u64,
        /// Threads creating files.
        #[arg(long)]
        threads: Option<usize>,
    },
}

fn create(plan: &Plan, root: &Path, threads: usize) -> anyhow::Result<()> {
    // Parents come before their children in the plan.
    let paths: Vec<PathBuf> = (0..index(plan.dirs.len()))
        .map(|d| plan.path(root, d))
        .collect();
    fs::create_dir_all(root)?;
    for p in &paths[1..] {
        fs::create_dir(p).with_context(|| format!("mkdir {}", p.display()))?;
    }
    // Data files first, then the links to them.
    for pass in [false, true] {
        let next = AtomicUsize::new(0);
        std::thread::scope(|s| -> anyhow::Result<()> {
            let workers: Vec<_> = (0..threads)
                .map(|_| {
                    s.spawn(|| -> anyhow::Result<()> {
                        loop {
                            let d = next.fetch_add(1, Ordering::Relaxed);
                            if d >= plan.dirs.len() {
                                return Ok(());
                            }
                            create_files(plan, &paths, index(d), pass)?;
                        }
                    })
                })
                .collect();
            for w in workers {
                match w.join() {
                    Ok(r) => r?,
                    Err(p) => std::panic::resume_unwind(p),
                }
            }
            Ok(())
        })?;
    }
    Ok(())
}

fn create_files(plan: &Plan, paths: &[PathBuf], d: u32, links: bool) -> anyhow::Result<()> {
    let dir = &paths[d as usize];
    for (k, e) in plan.dirs[d as usize].entries.iter().enumerate() {
        let path = dir.join(plan.file_name(d, index(k)));
        match (e, links) {
            (Entry::Data(size), false) => {
                let mut f = fs::File::create(&path)?;
                if *size <= 512 {
                    f.write_all(&vec![b'x'; usize::try_from(*size)?])?;
                } else {
                    f.set_len(*size)?;
                }
            }
            (Entry::HardLink(ld, lf), true) => {
                let target = paths[*ld as usize].join(plan.file_name(*ld, *lf));
                fs::hard_link(&target, &path)
                    .with_context(|| format!("link {}", path.display()))?;
            }
            (Entry::Symlink(t), true) => symlink(t, &path)?,
            _ => {}
        }
    }
    Ok(())
}

#[cfg(unix)]
fn symlink(target: &str, path: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, path)
}

/// Creating symlinks on Windows needs a privilege most users lack; a small file stands in.
#[cfg(windows)]
fn symlink(target: &str, path: &Path) -> std::io::Result<()> {
    fs::write(path, target)
}

fn main() -> anyhow::Result<()> {
    let Cmd::Gen {
        dir,
        entries,
        seed,
        wide,
        threads,
    } = Cli::parse().cmd;
    if dir.exists() && fs::read_dir(&dir)?.next().is_some() {
        bail!("{} exists and is not empty", dir.display());
    }
    let t0 = std::time::Instant::now();
    let plan = Plan::generate(entries, seed, wide);
    let (mut files, mut links, mut symlinks, mut bytes) = (0u64, 0u64, 0u64, 0u64);
    for d in &plan.dirs {
        for e in &d.entries {
            match e {
                Entry::Data(s) => {
                    files += 1;
                    bytes += s;
                }
                Entry::HardLink(..) => links += 1,
                Entry::Symlink(_) => symlinks += 1,
            }
        }
    }
    let threads = threads
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(4, |n| n.get().min(16)));
    create(&plan, &dir, threads)?;
    eprintln!(
        "{}: {} directories, {files} files ({bytes} bytes apparent), {links} hard links, \
         {symlinks} symlinks; seed {seed}; {:.1}s",
        dir.display(),
        plan.dirs.len() - 1,
        t0.elapsed().as_secs_f64()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_the_planned_tree() {
        let tmp = std::env::temp_dir().join(format!("dfdu-bench-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        let plan = Plan::generate(3000, 3, 200);
        create(&plan, &tmp, 4).unwrap();
        let mut n = 0u64;
        let mut stack = vec![tmp.clone()];
        while let Some(d) = stack.pop() {
            for e in fs::read_dir(&d).unwrap() {
                let e = e.unwrap();
                n += 1;
                if e.file_type().unwrap().is_dir() {
                    stack.push(e.path());
                }
            }
        }
        fs::remove_dir_all(&tmp).unwrap();
        assert_eq!(n, 3000);
    }
}

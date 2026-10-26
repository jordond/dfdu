//! `dut-bench gen`: a seeded synthetic tree for benchmarks (ROADMAP M2). The same seed and
//! size give the same tree on every machine and with any number of threads, so timings taken on
//! different days or machines compare like with like.
//!
//! The shape imitates a home or source directory: most directories are small, a few are wide,
//! file sizes are spread over six orders of magnitude, and about 1% of files are hard links and
//! 0.5% symlinks. Files above 512 bytes are sparse (`set_len`), so a 10M-entry tree takes a
//! few GiB of disk rather than terabytes; scan cost depends on the number of entries, not on
//! their contents.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use rand::rngs::ChaCha8Rng;
use rand::{RngExt, SeedableRng};

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

enum Entry {
    /// A regular file of this apparent size.
    Data(u64),
    /// A hard link to file `.1` of directory `.0` (always a `Data` entry).
    HardLink(u32, u32),
    /// A symlink with this target, relative to its directory.
    Symlink(String),
}

struct Dir {
    parent: u32,
    name: String,
    entries: Vec<Entry>,
}

struct Plan {
    dirs: Vec<Dir>,
}

impl Plan {
    fn generate(entries: u64, seed: u64, wide: u64) -> Plan {
        let mut rng = ChaCha8Rng::seed_from_u64(seed);
        let mut dirs = vec![Dir {
            parent: 0,
            name: String::new(),
            entries: vec![],
        }];
        let mut depth = vec![0u32];
        // (dir, file) of every data file so far, for hard links.
        let mut data: Vec<(u32, u32)> = vec![];
        let mut left = entries;
        if wide > 0 && left > 0 {
            left -= 1;
            dirs.push(Dir {
                parent: 0,
                name: "wide".into(),
                entries: vec![],
            });
            depth.push(1);
            let n = wide.min(left);
            for i in 0..n {
                dirs[1].entries.push(Entry::Data(file_size(&mut rng)));
                data.push((1, i as u32));
            }
            left -= n;
        }
        let mut queue = std::collections::VecDeque::from([0u32]);
        while left > 0 {
            let d = match queue.pop_front() {
                Some(d) => d,
                // The tree ran out of open directories: grow one below a random existing one.
                None => {
                    let p = rng.random_range(0..dirs.len() as u32);
                    let p = if dirs.len() > 1 && p == 1 && wide > 0 {
                        0
                    } else {
                        p
                    };
                    queue.push_back(p);
                    continue;
                }
            };
            let nsub = if depth[d as usize] >= 14 {
                0
            } else {
                match rng.random_range(0..100) {
                    0..35 => 0,
                    35..60 => 1,
                    60..75 => 2,
                    75..95 => rng.random_range(3..9),
                    _ => rng.random_range(9..40),
                }
            };
            let nfiles = match rng.random_range(0..100) {
                0..80 => rng.random_range(0..16),
                80..98 => rng.random_range(16..200),
                _ => rng.random_range(200..3000),
            };
            for _ in 0..nsub {
                if left == 0 {
                    break;
                }
                left -= 1;
                let i = dirs[d as usize].entries.len() + dirs.len();
                let id = dirs.len() as u32;
                dirs.push(Dir {
                    parent: d,
                    name: format!("d{i:x}{}", suffix(&mut rng)),
                    entries: vec![],
                });
                depth.push(depth[d as usize] + 1);
                queue.push_back(id);
            }
            for _ in 0..nfiles {
                if left == 0 {
                    break;
                }
                left -= 1;
                let k = dirs[d as usize].entries.len() as u32;
                let e = match rng.random_range(0..1000) {
                    0..10 if !data.is_empty() => {
                        let (ld, lf) = data[rng.random_range(0..data.len())];
                        Entry::HardLink(ld, lf)
                    }
                    10..15 => Entry::Symlink(if rng.random_bool(0.5) {
                        "..".into()
                    } else {
                        format!("missing-{k}")
                    }),
                    _ => {
                        data.push((d, k));
                        Entry::Data(file_size(&mut rng))
                    }
                };
                dirs[d as usize].entries.push(e);
            }
        }
        Plan { dirs }
    }

    fn path(&self, root: &Path, d: u32) -> PathBuf {
        let mut parts = vec![];
        let mut cur = d;
        while cur != 0 {
            parts.push(&self.dirs[cur as usize].name);
            cur = self.dirs[cur as usize].parent;
        }
        let mut p = root.to_path_buf();
        for n in parts.iter().rev() {
            p.push(n);
        }
        p
    }

    /// Hard links and data files share a name scheme, so a link's target path is known.
    fn file_name(&self, d: u32, k: u32) -> String {
        match self.dirs[d as usize].entries[k as usize] {
            Entry::Data(_) => format!("f{k:x}.dat"),
            Entry::HardLink(..) => format!("h{k:x}.dat"),
            Entry::Symlink(_) => format!("l{k:x}"),
        }
    }
}

/// Log-uniform from 1 byte to 4 MiB, plus some empty files.
fn file_size(rng: &mut ChaCha8Rng) -> u64 {
    if rng.random_range(0..20) == 0 {
        return 0;
    }
    let x: f64 = rng.random_range(0.0..(4.0 * 1024.0 * 1024.0f64).ln());
    x.exp() as u64
}

/// Names of different lengths, as real trees have.
fn suffix(rng: &mut ChaCha8Rng) -> String {
    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789_-";
    let n = rng.random_range(0..24);
    (0..n)
        .map(|_| CHARS[rng.random_range(0..CHARS.len())] as char)
        .collect()
}

fn create(plan: &Plan, root: &Path, threads: usize) -> anyhow::Result<()> {
    // Parents come before their children in the plan.
    let paths: Vec<PathBuf> = (0..plan.dirs.len() as u32)
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
                            create_files(plan, &paths, d as u32, pass)?;
                        }
                    })
                })
                .collect();
            for w in workers {
                w.join().expect("worker")?;
            }
            Ok(())
        })?;
    }
    Ok(())
}

fn create_files(plan: &Plan, paths: &[PathBuf], d: u32, links: bool) -> anyhow::Result<()> {
    let dir = &paths[d as usize];
    for (k, e) in plan.dirs[d as usize].entries.iter().enumerate() {
        let path = dir.join(plan.file_name(d, k as u32));
        match (e, links) {
            (Entry::Data(size), false) => {
                let mut f = fs::File::create(&path)?;
                if *size <= 512 {
                    f.write_all(&vec![b'x'; *size as usize])?;
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
    fn plan_has_the_requested_size_and_is_deterministic() {
        let count = |p: &Plan| {
            (p.dirs.len() - 1) as u64 + p.dirs.iter().map(|d| d.entries.len() as u64).sum::<u64>()
        };
        for (n, wide) in [(0, 0), (1, 0), (5000, 0), (5000, 3000), (100, 500)] {
            let a = Plan::generate(n, 7, wide);
            assert_eq!(count(&a), n, "entries {n} wide {wide}");
            let b = Plan::generate(n, 7, wide);
            let names = |p: &Plan| p.dirs.iter().map(|d| d.name.clone()).collect::<Vec<_>>();
            assert_eq!(names(&a), names(&b));
        }
    }

    #[test]
    fn creates_the_planned_tree() {
        let tmp = std::env::temp_dir().join(format!("dut-bench-test-{}", std::process::id()));
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

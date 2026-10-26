//! The tree `dut-bench gen` creates, as a plan in memory ([`Plan`]), and a scanner backend that
//! serves a plan without creating it ([`synth::SynthBackend`]), so the applier can be measured
//! on 10M entries without 10M inodes on disk.
//!
//! The shape imitates a home or source directory: most directories are small, a few are wide,
//! file sizes are spread over six orders of magnitude, and about 1% of files are hard links and
//! 0.5% symlinks.

use std::path::{Path, PathBuf};

use rand::rngs::ChaCha8Rng;
use rand::{RngExt, SeedableRng};

pub mod synth;

pub enum Entry {
    /// A regular file of this apparent size.
    Data(u64),
    /// A hard link to file `.1` of directory `.0` (always a `Data` entry).
    HardLink(u32, u32),
    /// A symlink with this target, relative to its directory.
    Symlink(String),
}

pub struct Dir {
    pub parent: u32,
    pub name: String,
    pub entries: Vec<Entry>,
}

pub struct Plan {
    pub dirs: Vec<Dir>,
}

impl Plan {
    pub fn generate(entries: u64, seed: u64, wide: u64) -> Plan {
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

    pub fn path(&self, root: &Path, d: u32) -> PathBuf {
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
    pub fn file_name(&self, d: u32, k: u32) -> String {
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
}

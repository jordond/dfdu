//! A bare FIEMAP loop: the floor for extent measurement (ROADMAP MB, `docs/btrfs.md` §3.4).
//!
//! Lists the regular files with allocated blocks below a directory, staying on its mount and
//! entering btrfs subvolumes (the files `dut` measures), then reads every file's extent map with
//! N threads: open the directory once, open each file relative to it, FIEMAP, close. Nothing
//! else is done with the result. With `--tree-search` each run is followed by one that also
//! reads each file's extent items from its subvolume tree, as exact mode does as root. Prints
//! the listing time and the time of each loop.
//!
//! ```text
//! cargo run --release -p dut-sys --example fiemap_loop -- <dir> [--threads N] [--runs R] [--tree-search]
//! ```

#[cfg(target_os = "linux")]
fn main() -> std::io::Result<()> {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("Linux only");
}

#[cfg(target_os = "linux")]
mod linux {
    use std::ffi::OsString;
    use std::io;
    use std::os::unix::fs::MetadataExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    use rustix::fs::{CWD, Mode, OFlags};

    pub fn main() -> io::Result<()> {
        let mut args = std::env::args_os().skip(1);
        let mut root = None;
        let mut threads = std::thread::available_parallelism().map_or(4, |n| n.get());
        let mut runs = 1;
        let mut tree_search = false;
        while let Some(a) = args.next() {
            match a.to_str() {
                Some("--threads") => threads = parse(args.next()),
                Some("--runs") => runs = parse(args.next()),
                Some("--tree-search") => tree_search = true,
                _ => root = Some(PathBuf::from(a)),
            }
        }
        let root = root.expect("usage: fiemap_loop <dir> [--threads N] [--runs R] [--tree-search]");

        let t = Instant::now();
        let dirs = list(root)?;
        let files: usize = dirs.iter().map(|d| d.1.len()).sum();
        println!(
            "listed {files} files in {} directories in {:.3} s",
            dirs.len(),
            t.elapsed().as_secs_f64()
        );
        for _ in 0..runs {
            for search in [false, true] {
                if search && !tree_search {
                    continue;
                }
                let t = Instant::now();
                let extents = measure(&dirs, threads, search);
                println!(
                    "{threads} threads{}: {files} files, {extents} extents in {:.3} s",
                    if search { ", tree search" } else { "" },
                    t.elapsed().as_secs_f64()
                );
            }
        }
        Ok(())
    }

    fn parse(a: Option<OsString>) -> usize {
        a.and_then(|a| a.to_str()?.parse().ok()).expect("a number")
    }

    /// Directories with the names of their regular files that have allocated blocks.
    fn list(root: PathBuf) -> io::Result<Vec<(PathBuf, Vec<OsString>)>> {
        let mount = dut_sys::mount_id(&root)?;
        let mut out = vec![];
        let mut stack = vec![root];
        while let Some(d) = stack.pop() {
            let mut names = vec![];
            for e in std::fs::read_dir(&d)? {
                let e = e?;
                let ft = e.file_type()?;
                if ft.is_dir() {
                    let p = e.path();
                    if dut_sys::mount_id(&p)? == mount {
                        stack.push(p);
                    }
                } else if ft.is_file() && e.metadata()?.blocks() > 0 {
                    names.push(e.file_name());
                }
            }
            if !names.is_empty() {
                out.push((d, names));
            }
        }
        Ok(out)
    }

    /// Reads every file's extent map; returns the number of extents.
    fn measure(dirs: &[(PathBuf, Vec<OsString>)], threads: usize, tree_search: bool) -> usize {
        let next = AtomicUsize::new(0);
        let total = AtomicUsize::new(0);
        std::thread::scope(|s| {
            for _ in 0..threads {
                s.spawn(|| {
                    let mut reader = dut_sys::ExtentReader::new();
                    let mut n = 0;
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some((dir, names)) = dirs.get(i) else {
                            break;
                        };
                        let Ok(dfd) = rustix::fs::openat(
                            CWD,
                            dir,
                            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
                            Mode::empty(),
                        ) else {
                            continue;
                        };
                        for name in names {
                            let flags = OFlags::RDONLY
                                | OFlags::NOFOLLOW
                                | OFlags::CLOEXEC
                                | OFlags::NOATIME;
                            let Ok(fd) = rustix::fs::openat(&dfd, name, flags, Mode::empty())
                            else {
                                continue;
                            };
                            n += reader.fiemap(&fd).map_or(0, |e| e.len());
                            if tree_search && let Ok(st) = rustix::fs::fstat(&fd) {
                                n += reader.file_extents(&fd, st.st_ino).map_or(0, |e| e.len());
                            }
                        }
                    }
                    total.fetch_add(n, Ordering::Relaxed);
                });
            }
        });
        total.into_inner()
    }
}

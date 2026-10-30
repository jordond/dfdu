//! Classifying a tree loaded from a snapshot without scanning it again (an incremental resume
//! rescans only what changed, so the rest must get its classes another way). Each directory's
//! children go through the scan workers' [`Listing`] as one whole block, so the classes and
//! protection come out as a scan would set them; only the marker files a rule reads a prefix
//! of are read from disk. It runs a step at a time between the applier's other work, so the
//! model's write lock is held for one bounded step at a time.

use std::cell::OnceCell;

use dfdu_scan::classify::{BlockParts, ClassOpts, Listing, TaskClass};
use dfdu_scan::{Backend, DirId, Entry, FileRec, Mark, NameSpan, SubdirRec};
use dfdu_tree::{Ix, Kind, NodeRef, Tree};

use crate::applier::meta_of;

/// Classifies a subtree against a job's [`ClassOpts`] a step at a time, top down.
pub(crate) struct ClassifyLoaded {
    stack: Vec<(NodeRef, TaskClass)>,
    names: Vec<u8>,
    files: Vec<FileRec>,
    subdirs: Vec<SubdirRec>,
    others: Vec<NameSpan>,
    file_ix: Vec<Ix>,
    subdir_ix: Vec<Ix>,
}

impl ClassifyLoaded {
    /// The classification of the subtree at `root` against `opts`.
    pub(crate) fn new(tree: &Tree, root: Ix, opts: &ClassOpts) -> Self {
        ClassifyLoaded {
            stack: vec![(tree.node_ref(root), TaskClass::root(opts))],
            names: Vec::new(),
            files: Vec::new(),
            subdirs: Vec::new(),
            others: Vec::new(),
            file_ix: Vec::new(),
            subdir_ix: Vec::new(),
        }
    }

    /// Classifies whole directories, calling `each_dir` on each as it is reached, until their
    /// entries reach `units`; whether none is left. A directory no longer visible is passed
    /// over with what is below it.
    pub(crate) fn step<B: Backend>(
        &mut self,
        tree: &mut Tree,
        opts: &ClassOpts,
        backend: &B,
        units: usize,
        mut each_dir: impl FnMut(&mut Tree, Ix),
    ) -> bool {
        let mut done = 0;
        while done < units
            && let Some((dir, task)) = self.stack.pop()
        {
            let Some(dir) = tree.resolve_visible(dir) else {
                continue;
            };
            each_dir(tree, dir);
            done += 1 + self.dir(tree, dir, task, opts, backend);
        }
        self.stack.is_empty()
    }

    /// Classifies the entries of `dir` and queues its subdirectories; returns how many entries
    /// it has.
    fn dir<B: Backend>(
        &mut self,
        tree: &mut Tree,
        dir: Ix,
        task: TaskClass,
        opts: &ClassOpts,
        backend: &B,
    ) -> usize {
        let ClassifyLoaded {
            stack,
            names,
            files,
            subdirs,
            others,
            file_ix,
            subdir_ix,
        } = self;
        names.clear();
        files.clear();
        subdirs.clear();
        others.clear();
        file_ix.clear();
        subdir_ix.clear();
        for c in tree.children(dir) {
            let n = tree.name(c);
            #[expect(
                clippy::cast_possible_truncation,
                reason = "one directory's names take far less than 4 GiB"
            )]
            let off = names.len() as u32;
            let name = NameSpan {
                off,
                len: u16::try_from(n.len()).unwrap_or(u16::MAX),
            };
            names.extend_from_slice(&n[..name.len as usize]);
            match tree.kind(c) {
                Kind::Dir => {
                    subdirs.push(SubdirRec {
                        name,
                        meta: meta_of(tree, c),
                        mount_root: false,
                        skip: None,
                    });
                    subdir_ix.push(c);
                }
                // An entry that could not be stat'ed is only a name to the rules.
                _ if tree.error(c).is_some() => others.push(name),
                _ => {
                    files.push(FileRec {
                        name,
                        meta: meta_of(tree, c),
                    });
                    file_ix.push(c);
                }
            }
        }
        let loc = OnceCell::new();
        let read = |n: &[u8], len: usize| {
            let loc = loc.get_or_init(|| backend.loc_from_path(&tree.path_buf(dir)));
            backend.read_prefix(loc, n, len)
        };
        let missing = |n: &[u8]| {
            let loc = loc.get_or_init(|| backend.loc_from_path(&tree.path_buf(dir)));
            backend.link_target_missing(loc, n)
        };
        let listing = Listing::new(opts.rules.clone(), DirId(0), task);
        let parts = BlockParts {
            names,
            files,
            subdirs,
            others,
            first_subdir: DirId(1),
            whole: true,
        };
        let out = listing.block(&parts, &read, &missing);
        let closes = listing.finish();
        for (e, mark) in out.marks {
            let node = match e {
                Entry::File(i) => file_ix[i as usize],
                Entry::Subdir(i) => subdir_ix[i as usize],
            };
            match mark {
                Mark::Class(class) => tree.set_class(node, Some(class)),
                Mark::Protected => tree.set_protection(node, true, false),
                Mark::Excepted => tree.set_protection(node, false, true),
            }
        }
        for (d, class) in closes {
            let node = match d.0 {
                0 => dir,
                i => subdir_ix[i as usize - 1],
            };
            tree.set_class(node, Some(class));
        }
        for (&s, t) in subdir_ix.iter().zip(out.subdirs) {
            if let Some(t) = t {
                stack.push((tree.node_ref(s), *t));
            }
        }
        files.len() + subdirs.len() + others.len()
    }
}

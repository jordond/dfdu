//! Classifying a tree loaded from a snapshot without scanning it again (an incremental resume
//! rescans only what changed, so the rest must get its classes another way). Each directory's
//! children go through the scan workers' [`Listing`] as one whole block, so the classes and
//! protection come out as a scan would set them; only the marker files a rule reads a prefix
//! of are read from disk.

use std::cell::OnceCell;

use dfdu_scan::classify::{ClassOpts, Listing, TaskClass};
use dfdu_scan::{Backend, DirId, Entry, FileRec, Mark, NameSpan, SubdirRec};
use dfdu_tree::{Ix, Kind, Tree};

use crate::applier::meta_of;

/// Classifies the subtree at `root` against `opts`, calling `each_dir` on every directory.
pub(crate) fn classify_loaded<B: Backend>(
    tree: &mut Tree,
    root: Ix,
    opts: &ClassOpts,
    backend: &B,
    mut each_dir: impl FnMut(&mut Tree, Ix),
) {
    let mut stack = vec![(root, TaskClass::root(opts))];
    let mut names = Vec::new();
    let (mut files, mut subdirs, mut others) = (Vec::new(), Vec::new(), Vec::new());
    let (mut file_ix, mut subdir_ix) = (Vec::new(), Vec::new());
    while let Some((dir, task)) = stack.pop() {
        each_dir(tree, dir);
        names.clear();
        files.clear();
        subdirs.clear();
        others.clear();
        file_ix.clear();
        subdir_ix.clear();
        for c in tree.children(dir) {
            let n = tree.name(c);
            let name = NameSpan {
                off: names.len() as u32,
                len: n.len().min(u16::MAX as usize) as u16,
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
        let out = listing.block(
            &names,
            &files,
            &subdirs,
            &others,
            DirId(1),
            true,
            &read,
            &missing,
        );
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
                stack.push((s, *t));
            }
        }
    }
}

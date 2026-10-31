//! Trees on disk loaded as a snapshot of them would be.

use std::fs;
use std::path::Path;

use dfdu_scan::{Backend, PortableBackend};
use dfdu_tree::{DirState, Ix, Tree};

/// The tree of `root` as a snapshot of it now would load: every directory complete, a hard
/// link charged where it is first met.
pub fn loaded(root: &Path) -> Tree {
    fn add(t: &mut Tree, dir: Ix, path: &Path, seen: &mut Vec<(u64, u64)>) {
        let mut entries: Vec<_> = fs::read_dir(path).unwrap().map(|e| e.unwrap()).collect();
        entries.sort_by_key(fs::DirEntry::file_name);
        for e in entries {
            let file_name = e.file_name();
            let name = dfdu_tree::name_bytes(&file_name);
            let meta = PortableBackend::default().stat_dir(&e.path()).unwrap();
            if e.file_type().unwrap().is_dir() {
                let d = t.add_dir(dir, &name, &meta);
                let own = t.own(d);
                t.propagate_add(dir, &own);
                add(t, d, &e.path(), seen);
                close(t, d);
            } else {
                let key = (meta.dev, meta.ino);
                let charged = !meta.is_hardlink() || !seen.contains(&key);
                seen.push(key);
                let f = t.add_file(dir, &name, &meta, charged, None);
                let own = t.own(f);
                t.propagate_add(dir, &own);
            }
        }
    }
    fn close(t: &mut Tree, d: Ix) {
        let r = t.dir_mut(d).unwrap();
        r.state = DirState::Complete;
        r.outcome = r.state;
        r.pending = 0;
        r.listing_closed = true;
        r.reported = true;
    }
    let mut t = Tree::default();
    let meta = PortableBackend::default().stat_dir(root).unwrap();
    let r = t.new_root(&dfdu_tree::name_bytes(root.as_os_str()), &meta, true);
    add(&mut t, r, root, &mut Vec::new());
    close(&mut t, r);
    t
}

/// The node at `rel` below the root, if there is one.
pub fn find(t: &Tree, rel: &str) -> Option<Ix> {
    let mut at = t.roots().next().unwrap();
    for n in rel.split('/').filter(|n| !n.is_empty()) {
        at = t.children(at).find(|&c| t.name(c) == n.as_bytes())?;
    }
    Some(at)
}

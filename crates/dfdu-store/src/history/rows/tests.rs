use dfdu_tree::{Class, Kind, Meta, RuleIx, Safety};

use super::*;

fn meta(kind: Kind, size: u64) -> Meta {
    Meta {
        kind,
        apparent: size,
        alloc: size,
        mtime: 0,
        dev: 1,
        ino: 1,
        nlink: 1,
        mount: 0,
    }
}

fn add(t: &mut Tree, parent: Ix, name: &str, kind: Kind, size: u64) -> Ix {
    let m = meta(kind, size);
    let ix = match kind {
        Kind::Dir => t.add_dir(parent, name.as_bytes(), &m),
        _ => t.add_file(parent, name.as_bytes(), &m, false, None),
    };
    let own = t.own(ix);
    t.propagate_add(parent, &own);
    ix
}

const CLASS: Class = Class {
    rule: RuleIx(0),
    safety: Safety::Safe,
    degraded: None,
    flags: dfdu_tree::ClassFlags::NONE,
};

fn p(s: &str) -> Vec<u8> {
    s.replace('/', std::path::MAIN_SEPARATOR_STR).into_bytes()
}

/// `/r/{a/{f, big/{x}, small/{m.txt, matched/}}}`, with `a`, `m.txt` and `matched` classified,
/// and a classified directory under another root.
fn sample() -> (Tree, Ix) {
    let mut t = Tree::new();
    let root = t.new_root(b"/r", &meta(Kind::Dir, 0), true);
    let a = add(&mut t, root, "a", Kind::Dir, 0);
    add(&mut t, a, "f", Kind::File, 7);
    let big = add(&mut t, a, "big", Kind::Dir, 0);
    add(&mut t, big, "x", Kind::File, 5000);
    let small = add(&mut t, a, "small", Kind::Dir, 0);
    let m = add(&mut t, small, "m.txt", Kind::File, 20);
    let matched = add(&mut t, small, "matched", Kind::Dir, 10);
    let other = t.new_root(b"/o", &meta(Kind::Dir, 0), true);
    let elsewhere = add(&mut t, other, "e", Kind::Dir, 1);
    for ix in [a, m, matched, elsewhere] {
        t.set_class(ix, Some(CLASS));
    }
    (t, root)
}

#[test]
fn keeps_shallow_and_big_directories_and_every_match_below_the_root() {
    let (t, root) = sample();
    let got = rows_to(&t, root, 1, 1000, usize::MAX);
    let keys: Vec<Vec<u8>> = got.keys().cloned().collect();
    assert_eq!(
        keys,
        [
            p(""),
            p("a"),
            p("a/big"),
            p("a/small/m.txt"),
            p("a/small/matched")
        ]
    );
    assert_eq!(
        got[&p("a/big")],
        Sizes {
            disk: 5000,
            apparent: 5000,
            entries: 2
        }
    );
    assert_eq!(got[&p("a/small/m.txt")].disk, 20);
}

/// Past `max` rows the smallest go first.
#[test]
fn the_largest_rows_are_kept() {
    let (t, root) = sample();
    let got = rows_to(&t, root, 1, 1000, 3);
    let keys: Vec<Vec<u8>> = got.keys().cloned().collect();
    assert_eq!(keys, [p(""), p("a"), p("a/big")]);
}

#[test]
fn rel_path_is_empty_for_the_root_and_joined_below_it() {
    let (t, root) = sample();
    let a = t.children(root).next().unwrap();
    let small = t.children(a).nth(2).unwrap();
    let m = t.children(small).next().unwrap();
    assert_eq!(rel_path(&t, root, root), b"");
    assert_eq!(rel_path(&t, m, root), p("a/small/m.txt"));
    assert_eq!(rel_path(&t, m, a), p("small/m.txt"));
}

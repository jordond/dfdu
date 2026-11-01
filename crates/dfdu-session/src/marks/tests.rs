use std::time::{Duration, Instant};

use dfdu_engine::test_support::{add_dir, add_file};
use dfdu_tree::Kind;

use super::*;
use crate::test_support::root_model;

/// A mark of `path` with no reference, as after a rescan swap.
fn lost(path: &str, ident: Option<Ident>) -> Item {
    Item {
        target: Target {
            node: None,
            path: path.into(),
            ident,
        },
        is_dir: false,
        size: 1,
        items: 1,
        shared_out: 0,
        scanning: false,
        inside: false,
    }
}

#[test]
fn many_marks_index_quickly() {
    let mut marks = Marks::default();
    for i in 0..10_000 {
        let path = format!("/home/jo/d{}/e{}/f{i}", i % 100, i % 7);
        marks.items.push(lost(&path, None));
    }
    marks.items.push(lost("/home/jo/d1", None));
    let t = Instant::now();
    marks.index();
    let took = t.elapsed();
    println!("10,001 marks indexed in {took:?}");
    assert!(took < Duration::from_secs(2), "{took:?}");
    assert_eq!(marks.items.iter().filter(|it| it.inside).count(), 100);
    assert_eq!(marks.total(), (10_000 - 100 + 1, false));
}

#[test]
fn lost_references_are_found_by_path() {
    let (mut m, root) = root_model();
    let big = add_dir(&mut m.tree, root, "big", 2);
    let files: Vec<Ix> = (0..1000)
        .map(|i| add_file(&mut m.tree, big, &format!("f{i}"), 10, 100 + i))
        .collect();
    let other = add_dir(&mut m.tree, root, "other", 3);
    let g = add_file(&mut m.tree, other, "g", 10, 4);
    let file = |ino| {
        Some(Ident {
            dev_ino: (1, ino),
            kind: Kind::File,
        })
    };
    let mut s = SessionState::new(Instant::now());
    s.marks.items = vec![
        lost("/r/big/f999", file(1099)),
        lost("/r/other/g", file(4)),
        lost("/r/big/f10", None),
        lost("/r/big/missing", None),
        lost("/r/big/f20", file(7)),
        lost("/r/gone/f1", None),
        lost("/r/big/f500", file(600)),
        lost("/r", None),
    ];
    let found: Vec<_> = read(&m, &s)
        .found
        .iter()
        .map(|f| f.as_ref().map(|f| f.node))
        .collect();
    let at = |ix| Some(m.tree.node_ref(ix));
    assert_eq!(
        found,
        [
            at(files[999]),
            at(g),
            at(files[10]),
            None,
            None,
            None,
            at(files[500]),
            at(root),
        ]
    );
}

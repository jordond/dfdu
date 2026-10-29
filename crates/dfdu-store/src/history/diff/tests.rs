use super::super::tests::map;
use super::*;

fn p(s: &str) -> String {
    s.replace('/', std::path::MAIN_SEPARATOR_STR)
}

#[test]
fn diff_lists_changes_biggest_first() {
    let old = map(&[("", 100), (&p("a"), 60), (&p("a/b"), 10), (&p("gone"), 5)]);
    let mut new = map(&[("", 300), (&p("a"), 60), (&p("c"), 40)]);
    new.insert(p("a/b/c/d/e/deep").into_bytes(), Sizes::default());
    let d = diff(&old, &new, 4);
    let got: Vec<(String, i128)> = d
        .iter()
        .map(|c| (String::from_utf8(c.path.clone()).unwrap(), c.delta()))
        .collect();
    assert_eq!(
        got,
        [
            (String::new(), 200),
            ("c".into(), 40),
            (p("a/b"), -10),
            ("gone".into(), -5)
        ]
    );
    assert_eq!(d[1].old, None);
    assert_eq!(d[2].new, None);
    assert_eq!(depth(p("a/b/c").as_bytes()), 3);
    assert_eq!(depth(b""), 0);
}

/// A path only on one side below the complete levels is not reported either way; one whose
/// apparent size or entries changed but not its disk usage is, after the disk changes, by path.
#[test]
fn deep_one_sided_paths_are_skipped_and_ties_go_by_path() {
    let old = map(&[(&p("x/y"), 50), ("b", 8), ("a", 8)]);
    let mut new = map(&[(&p("x/z"), 70), ("b", 8), ("a", 8), ("c", 1)]);
    for k in ["a", "b"] {
        new.get_mut(k.as_bytes()).unwrap().apparent += 3;
    }
    let d = diff(&old, &new, 1);
    let got: Vec<(&[u8], i128, i128)> = d
        .iter()
        .map(|c| (&c.path[..], c.delta(), c.apparent_delta()))
        .collect();
    assert_eq!(
        got,
        [(&b"c"[..], 1, 0), (&b"a"[..], 0, 3), (&b"b"[..], 0, 3)]
    );
    assert_eq!(diff(&old, &old, 9), []);
}

#[test]
fn deltas_count_a_missing_side_as_zero() {
    let s = Sizes {
        disk: 4096,
        apparent: 100,
        entries: 1,
    };
    let gone = Change {
        path: vec![],
        old: Some(s),
        new: None,
    };
    let came = Change {
        old: None,
        new: Some(s),
        ..gone.clone()
    };
    assert_eq!((gone.delta(), gone.apparent_delta()), (-4096, -100));
    assert_eq!((came.delta(), came.apparent_delta()), (4096, 100));
}

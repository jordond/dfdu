use super::*;
use crate::state::order::{SortSpec, tree_order};

/// The tree rows the tests of tree mode share, from [`dfdu_session::test_support`].
pub(crate) use dfdu_session::test_support::{sample, sample_with_files};

fn guides(es: &[EntryData]) -> Vec<String> {
    let rows = tree_order(es, SortSpec::default());
    assert_eq!(rows, (0..es.len()).collect::<Vec<_>>(), "preorder by size");
    let last = last_children(es, &rows);
    rows.iter().map(|&i| guide(es, &last, i)).collect()
}

#[test]
fn guides_for_nested_last_and_single_children() {
    let g = guides(&sample());
    assert_eq!(
        g,
        [
            "",
            "├─",
            "├─",
            "│ ├─",
            "│ └─",
            "└─", // a: nested, the last child closes it
            "",
            "└─",
            "  └─", // b: single children all the way down
        ]
    );
}

#[test]
fn guides_follow_the_display_order() {
    // z grows past y: y becomes the last of a's children, and z no longer is.
    let mut es = sample();
    es[5].size = 45;
    let rows = tree_order(&es, SortSpec::default());
    let names: Vec<&[u8]> = rows.iter().map(|&i| &es[i].name[..]).collect();
    assert_eq!(names[..4], [&b"a"[..], b"x", b"z", b"y"]);
    let last = last_children(&es, &rows);
    assert_eq!(guide(&es, &last, 5), "├─", "z is no longer last");
    assert_eq!(guide(&es, &last, 2), "└─");
    assert_eq!(
        guide(&es, &last, 3),
        "  ├─",
        "y is last: no line down past it"
    );
}

use std::time::Instant;

use super::*;
use crate::features::dupes::found;
use crate::features::dupes::tests::found_in;
use crate::theme::Theme;

fn text(line: &Line) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

/// A browser whose comparison found `/r/photos/a.jpg` (oldest), `/r/photos/old/a.jpg` and
/// `/r/photos/b.jpg` in one group, and `/r/x` and `/r/y` in another.
fn app() -> App {
    let mut app = App::new(Theme::no_color(), Instant::now());
    found_in(
        &mut app,
        &[
            &[
                ("/r/photos/a.jpg", 10),
                ("/r/photos/old/a.jpg", 20),
                ("/r/photos/b.jpg", 30),
            ],
            &[("/r/x", 5), ("/r/y", 6)],
        ],
    );
    app
}

fn candidate(links: u32, private: Option<u64>) -> Candidate {
    Candidate {
        path: "/r/a".into(),
        size: 1 << 20,
        alloc: 1 << 20,
        dev_ino: (1, 2),
        mtime: 0,
        links,
        private,
    }
}

#[test]
fn paths_are_below_the_root_and_the_directory_they_share() {
    let root = Path::new("/r");
    assert_eq!(shown(Path::new("/r/a/b"), root), PathBuf::from("a/b"));
    assert_eq!(
        shown(root, root),
        PathBuf::from("/r"),
        "the root itself stays whole"
    );
    assert_eq!(shown(Path::new("/x/y"), root), PathBuf::from("/x/y"));
    let paths = |ps: &[&str]| ps.iter().map(PathBuf::from).collect::<Vec<_>>();
    assert_eq!(
        shared_dir(&paths(&["a/b/x", "a/b/y", "a/c"])),
        PathBuf::from("a")
    );
    assert_eq!(shared_dir(&paths(&["x", "y"])), PathBuf::new());
    assert_eq!(shared_dir(&[]), PathBuf::new());

    let app = app();
    let group = &found(&app).expect("a result").groups[0];
    let (dir, rest) = group_paths(&app, group, root);
    assert_eq!(dir, PathBuf::from("photos"));
    let rest: Vec<(usize, PathBuf)> = rest;
    assert_eq!(
        rest,
        [
            (0, PathBuf::from("a.jpg")),
            (1, PathBuf::from("old/a.jpg")),
            (2, PathBuf::from("b.jpg"))
        ]
    );
}

#[test]
fn the_progress_line_names_the_round() {
    let p = Progress::default();
    p.round.store(1, Ordering::Relaxed);
    p.files.store(3, Ordering::Relaxed);
    p.round_files.store(10, Ordering::Relaxed);
    p.bytes.store(1 << 20, Ordering::Relaxed);
    p.round_bytes.store(4 << 20, Ordering::Relaxed);
    assert_eq!(
        progress_line(&p),
        " comparing the starts and ends of files: 3 of 10 files, 1.0 MiB of 4.0 MiB"
    );
    p.round.store(2, Ordering::Relaxed);
    assert!(progress_line(&p).starts_with(" comparing the contents of files: "));
}

#[test]
fn what_deleting_a_copy_frees_is_said_in_words() {
    assert_eq!(
        frees(&candidate(1, None)),
        "has 1 other name (hard links): deleting this one frees nothing"
    );
    assert!(frees(&candidate(2, None)).starts_with("has 2 other names"));
    assert!(frees(&candidate(0, Some(0))).starts_with("a clone"));
    assert_eq!(
        frees(&candidate(0, Some(4096))),
        "shares some data: deleting it frees 4.0 KiB"
    );
    assert_eq!(frees(&candidate(0, None)), "deleting it frees 1.0 MiB");
}

#[test]
fn the_tally_counts_the_copies_but_the_kept_ones_unless_ticked() {
    let mut app = app();
    let (t, groups) = tally(&app, found(&app).expect("a result"));
    assert_eq!(groups, 2);
    assert_eq!((t.items, t.ticked, t.size), (3, 0, 3 << 20));
    // Ticking the kept copy counts it too.
    let group = found(&app).expect("a result").groups[0].clone();
    let c = &group.files[0];
    let node = super::live(&app, c).expect("in the tree");
    crate::state::marks::set(&mut app, super::super::mark_of(c, node), true);
    let (t, _) = tally(&app, found(&app).expect("a result"));
    assert_eq!((t.items, t.ticked, t.ticked_size), (4, 1, 1 << 20));
}

#[test]
fn rows_show_the_copy_kept_and_the_keep_rule() {
    let app = app();
    let f = found(&app).expect("a result");
    let group = &f.groups[0];
    let kept_row = text(&file_line(&app, group, 0, "a.jpg", 100, 60));
    assert!(kept_row.starts_with("   KEEP "), "{kept_row}");
    assert!(kept_row.ends_with("a.jpg"), "{kept_row}");
    let other = text(&file_line(&app, group, 1, "old/a.jpg", 100, 60));
    assert!(other.starts_with("   [ ] "), "{other}");
    let header = text(&group_line(&app, 0, group, Path::new("photos"), 3, 80));
    assert_eq!(
        header,
        format!(
            " ▾ 3 copies of 1.0 MiB · frees 2.0 MiB · in photos{}",
            std::path::MAIN_SEPARATOR
        )
    );
    let keep = keep_line(&app.theme, Keep::Newest);
    let current = keep
        .spans
        .iter()
        .find(|s| s.content == "newest")
        .expect("newest");
    assert!(current.style.add_modifier.contains(Modifier::REVERSED));
    assert!(text(&keep).ends_with("K change"), "{}", text(&keep));
}

#[test]
fn the_detail_pane_says_which_copy_is_kept_and_which_go() {
    let app = app();
    let f = found(&app).expect("a result");
    let lines: Vec<String> = detail_lines(&app, f, Some(&Row::File(0, 1)), Path::new("/r"))
        .iter()
        .map(text)
        .collect();
    assert!(
        lines[1].starts_with("1.0 MiB · modified 1970-01-01"),
        "{}",
        lines[1]
    );
    assert_eq!(lines[2], "deleting it frees 1.0 MiB");
    let labels: Vec<&str> = lines[4..].iter().map(|l| &l[..4]).collect();
    assert_eq!(labels, ["kept", "goes", "goes"]);
    // On a group's row, its copies only.
    let group = detail_lines(&app, f, Some(&Row::Group(1)), Path::new("/r"));
    assert_eq!(group.len(), 2);
    assert_eq!(detail_lines(&app, f, None, Path::new("/r")), Vec::<Line>::new());
}

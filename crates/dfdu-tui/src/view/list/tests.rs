use std::path::PathBuf;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::KeyCode;

use super::*;
use crate::app::Overlay;
use crate::model::snapshot::{EntryData, Snapshot, test_entry};
use crate::tests::{press, snap_without_totals};
use crate::theme::Theme;

/// [`snap_without_totals`] of a scan that has finished.
fn finished_snap(entries: Vec<EntryData>, version: u64) -> Snapshot {
    Snapshot {
        busy: false,
        ..snap_without_totals(entries, version)
    }
}

/// Rows note what grew or shrank noticeably since the last recorded scan; small changes,
/// entries the history does not have and rows still scanning get no note.
#[test]
fn rows_note_growth_since_the_last_scan() {
    use dfdu_store::history::{Growth, Sizes};
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    let mib = 1u64 << 20;
    let then = |disk: u64| Sizes {
        disk,
        apparent: disk,
        entries: 1,
    };
    app.growth = Some(std::sync::Arc::new(Growth {
        root: PathBuf::from("/r"),
        at: 86_400 * 3,
        sizes: [
            (b"grew".to_vec(), then(mib)),
            (b"shrank".to_vec(), then(9 * mib)),
            (b"same".to_vec(), then(4 * mib + 1000)),
            (b"busy".to_vec(), then(mib)),
        ]
        .into_iter()
        .collect(),
    }));
    let busy = EntryData {
        mark: Mark::Scanning,
        ..test_entry(4, "busy", 7 * mib)
    };
    app.refresh(
        finished_snap(
            vec![
                test_entry(0, "grew", 5 * mib),
                test_entry(1, "shrank", 2 * mib),
                test_entry(2, "same", 4 * mib),
                test_entry(3, "unknown", 3 * mib),
                busy,
            ],
            1,
        ),
        t0,
    );
    let mut term = Terminal::new(TestBackend::new(120, 10)).expect("terminal");
    let mut draw = |app: &mut App| {
        term.draw(|f| crate::view::draw(f, app, t0)).expect("draw");
        let buf = term.backend().buffer();
        (0..10)
            .map(|y| (0..120).map(|x| buf[(x, y)].symbol()).collect())
            .collect::<Vec<String>>()
    };
    let rows = draw(&mut app);
    let row = |name: &str| {
        rows.iter()
            .find(|r| r.contains(&format!("  {name}")))
            .unwrap_or_else(|| panic!("{name}: {rows:#?}"))
    };
    // The change has a column of its own, headed with the day of the last scan.
    assert!(rows[2].contains("Δ 01-04  name"), "{rows:#?}");
    assert!(row("grew").contains("  +4.0 MiB  grew"), "{rows:#?}");
    assert!(row("shrank").contains("  -7.0 MiB  shrank"), "{rows:#?}");
    for quiet in ["same", "unknown", "busy"] {
        assert!(
            !row(quiet).contains(&format!("MiB  {quiet}")),
            "{quiet}: {rows:#?}"
        );
    }
    // The selection line has it in full.
    app.key(press(KeyCode::Down), t0);
    let rows = draw(&mut app);
    assert!(
        rows[8].contains("▸ grew  5.0 MiB · +4.0 MiB since 1970-01-04"),
        "{rows:#?}"
    );
}

#[test]
fn ghost_rows_are_struck_through_and_cannot_be_opened_or_deleted() {
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    let dir = EntryData {
        is_dir: true,
        ..test_entry(0, "gone", 500)
    };
    app.refresh(
        finished_snap(vec![dir.clone(), test_entry(1, "kept", 10)], 1),
        t0,
    );
    // The deleted directory stays in its row as a ghost until the next re-sort.
    let ghost = EntryData {
        ghost: true,
        pseudo: true,
        is_dir: false,
        ..dir
    };
    app.refresh(finished_snap(vec![ghost, test_entry(1, "kept", 10)], 2), t0);
    assert_eq!(app.rows.len(), 2);
    assert!(app.selected_entry().expect("selected").ghost);

    let mut term = Terminal::new(TestBackend::new(80, 7)).expect("terminal");
    term.draw(|f| crate::view::draw(f, &mut app, t0))
        .expect("draw");
    let buf = term.backend().buffer();
    let row: String = (0..80).map(|x| buf[(x, 3)].symbol()).collect();
    assert!(row.contains("gone  deleted"), "{row}");
    let cells: Vec<&str> = (0..80).map(|x| buf[(x, 3)].symbol()).collect();
    let at = (0..76)
        .find(|&x| cells[x..x + 4].concat() == "gone")
        .expect("name");
    assert!(
        buf[(u16::try_from(at).unwrap(), 3)]
            .modifier
            .contains(Modifier::CROSSED_OUT),
        "{row}"
    );

    for code in [KeyCode::Enter, KeyCode::Char('d'), KeyCode::Char('D')] {
        assert!(app.key(press(code), t0).is_empty());
        assert!(matches!(app.overlay, Overlay::None), "{code:?}");
        assert_eq!(app.target.path, PathBuf::new(), "{code:?} opened the ghost");
    }
    // A re-sort drops the ghost.
    app.key(press(KeyCode::Char('S')), t0);
    app.reorder(t0);
    assert_eq!(app.rows.len(), 1);
}

#[test]
fn ghost_rows_have_no_share() {
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    let (gone, kept) = (test_entry(0, "gone", 4 << 20), test_entry(1, "kept", 1));
    app.refresh(finished_snap(vec![gone.clone(), kept.clone()], 1), t0);
    // 4 MiB deleted beside 1 byte left: the total no longer counts the ghost.
    let ghost = EntryData {
        ghost: true,
        pseudo: true,
        ..gone
    };
    let mut s = finished_snap(vec![ghost, kept], 2);
    s.dir_total.alloc = 1;
    app.refresh(s, t0);
    assert_eq!(app.rows.len(), 2);
    let out = crate::tests::render(&mut app, t0, 80, 7);
    let row = |name: &str| {
        out.lines()
            .find(|l| l.contains(name))
            .expect("row")
            .to_string()
    };
    let (gone, kept) = (row("gone"), row("kept"));
    assert!(!gone.contains('%'), "{out}");
    assert!(kept.contains("100.0%"), "{out}");
    // The columns after the share stay in place.
    let column = |row: &str, name| row.split(name).next().map(|s| s.chars().count());
    assert_eq!(column(&gone, "gone"), column(&kept, "kept"), "{out}");
}

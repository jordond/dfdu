//! What grew since the last recorded scan of the root (`dfdu_store::history`): a note on list
//! rows whose size changed noticeably, and a line in the details pane.

use std::path::Path;

use dfdu_store::history;

use crate::app::App;
use crate::snapshot::{Mark, SizeMode};

/// Changes smaller than this are not noted on list rows.
const NOTICEABLE: u64 = 1 << 20;

/// The change in the size shown of `path` (now `size`) since the last recorded scan, with its
/// size then and when that scan finished. `None` when the history has no size for it, the
/// size mode is not recorded, or the size is still changing.
pub fn since(app: &App, path: &Path, size: u64, mark: Mark) -> Option<(i128, u64, i64)> {
    if matches!(
        mark,
        Mark::Scanning | Mark::Rescanning | Mark::Stalled | Mark::Incomplete | Mark::Deleting
    ) {
        return None;
    }
    let g = app.growth.as_ref()?;
    let then = g.get(path)?;
    let then = match app.size_mode {
        SizeMode::Disk => then.disk,
        SizeMode::Apparent => then.apparent,
        SizeMode::Referenced => return None,
    };
    Some((size as i128 - then as i128, then, g.at))
}

/// "+12.4 GiB since 2026-10-18".
pub fn describe(delta: i128, at: i64) -> String {
    let sign = if delta < 0 { "-" } else { "+" };
    let abs = delta.unsigned_abs().min(u64::MAX as u128) as u64;
    format!(
        "{sign}{} since {}",
        crate::theme::human(abs),
        history::day(at)
    )
}

/// The note for a list row: changes of at least 1 MiB and 1% of the size.
pub fn row_note(app: &App, path: &Path, size: u64, mark: Mark) -> Option<(String, bool)> {
    let (d, then, at) = since(app, path, size, mark)?;
    let abs = d.unsigned_abs();
    if abs < NOTICEABLE as u128 || abs * 100 < then.max(size) as u128 {
        return None;
    }
    Some((describe(d, at), d > 0))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{Duration, Instant};

    use crossbeam_channel::unbounded;
    use dfdu_engine::{Command, Engine, EngineOptions};
    use dfdu_scan::{JobOptions, PortableBackend};
    use dfdu_store::history::History;
    use ratatui::backend::CrosstermBackend;
    use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use ratatui::layout::Rect;
    use ratatui::{Terminal, TerminalOptions, Viewport};

    use crate::{HistoryLink, UiOptions};

    /// The browser records its scan in the history once the scan is done, and waits for the
    /// write when it quits.
    #[test]
    fn the_browser_records_its_scan() {
        let tmp = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(tmp.path()).unwrap().join("tree");
        fs::create_dir_all(root.join("a")).unwrap();
        fs::write(root.join("a/f"), vec![1u8; 50_000]).unwrap();
        let db = tmp.path().join("history.sqlite3");
        let engine = Engine::start(
            PortableBackend::default(),
            EngineOptions {
                workers: 2,
                journal_dir: None,
                ..Default::default()
            },
        );
        let (tx, input) = unbounded();
        let ui = UiOptions {
            history: Some(HistoryLink::start(db.clone(), root.clone(), "k".into())),
            ..UiOptions::from_env()
        };
        engine.send(Command::Scan {
            path: root.clone(),
            opts: JobOptions::default(),
        });
        let recorded = std::thread::scope(|s| {
            let ui = s.spawn(|| {
                let backend = CrosstermBackend::new(std::io::sink());
                let viewport = Viewport::Fixed(Rect::new(0, 0, 100, 30));
                let mut terminal = Terminal::with_options(backend, TerminalOptions { viewport })?;
                crate::event_loop_with(&mut terminal, &engine, input, ui)
            });
            let t0 = Instant::now();
            let scans = loop {
                let scans = History::open(&db)
                    .and_then(|h| h.scans(None))
                    .unwrap_or_default();
                if !scans.is_empty() || t0.elapsed() > Duration::from_secs(20) {
                    break scans;
                }
                std::thread::sleep(Duration::from_millis(20));
            };
            let q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
            tx.send(Event::Key(q)).unwrap();
            ui.join().unwrap().unwrap();
            scans
        });
        assert_eq!(recorded.len(), 1, "{recorded:?}");
        let s = &recorded[0];
        assert_eq!(s.root, root);
        assert_eq!(s.options, "k");
        assert!(s.complete);
        let total = engine.read(|m| m.tree.total(m.tree.roots().next().unwrap()));
        assert_eq!(s.total.disk, total.alloc);
        let sizes = History::open(&db).unwrap().sizes(s.id).unwrap();
        assert!(sizes.contains_key(&b"a"[..]), "{sizes:?}");
        engine.shutdown(Duration::from_millis(200));
    }
}

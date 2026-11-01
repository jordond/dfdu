//! What grew since the last recorded scan of the root, in `dfdu-session`
//! (`dfdu_session::growth`), read with the app's history and size mode.

pub use dfdu_session::growth::{RowNote, describe};
use dfdu_tree::HostPath;

use crate::app::App;
use crate::model::snapshot::Mark;

/// The change in the size shown of `path` (now `size`) since the last recorded scan, with its
/// size then and when that scan finished ([`dfdu_session::growth::since`]).
pub fn since(app: &App, path: &HostPath, size: u64, mark: Mark) -> Option<(i128, u64, i64)> {
    dfdu_session::growth::since(app.growth.as_deref(), app.size_mode, path, size, mark)
}

/// The note for a list row ([`dfdu_session::growth::row_note`]).
pub fn row_note(app: &App, path: &HostPath, size: u64, mark: Mark) -> Option<RowNote> {
    dfdu_session::growth::row_note(app.growth.as_deref(), app.size_mode, path, size, mark)
}

/// The header of the column of changes ([`dfdu_session::growth::column_title`]).
pub fn column_title(app: &App) -> String {
    dfdu_session::growth::column_title(app.growth.as_deref())
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
            path: root.clone().into(),
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

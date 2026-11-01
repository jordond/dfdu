//! The browser's link to the scan history, in `dfdu-session` (`dfdu_session::history_link`):
//! what a poll takes in goes to the app here, a problem to the status line.

use dfdu_engine::Engine;
pub use dfdu_session::history_link::HistoryLink;

use crate::app::App;

/// Records the scan once its first job is done ([`dfdu_session::history_link::Recorder`]), and
/// puts what the history has in the app.
pub(crate) struct Recorder(dfdu_session::history_link::Recorder);

impl Recorder {
    pub(crate) fn new(link: Option<HistoryLink>) -> Self {
        Recorder(dfdu_session::history_link::Recorder::new(link))
    }

    /// Takes in what the history has: the last scan's sizes once loaded, and records this scan
    /// once it is complete. A problem with either is put in the status line. True when there
    /// is something new to draw.
    pub(crate) fn poll<B: dfdu_scan::Backend>(
        &mut self,
        app: &mut App,
        engine: &Engine<B>,
    ) -> bool {
        let polled = self.0.poll(engine);
        if let Some(g) = &polled.growth {
            app.session.growth.clone_from(g);
        }
        if let Some(p) = &polled.problem {
            app.session.status = Some(p.clone());
        }
        polled.changed()
    }

    /// The history is still being read or written, so [`Recorder::poll`] has more to take in.
    pub(crate) fn busy(&self) -> bool {
        self.0.busy()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use dfdu_engine::EngineOptions;
    use dfdu_scan::PortableBackend;

    use super::*;
    use crate::theme::Theme;

    fn poll_until(rec: &mut Recorder, app: &mut App, engine: &Engine<PortableBackend>) -> bool {
        let t0 = std::time::Instant::now();
        while t0.elapsed() < Duration::from_secs(10) {
            if rec.poll(app, engine) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    /// A history that cannot be opened is reported in the status line, not taken for one
    /// without a previous scan.
    #[test]
    fn load_errors_are_reported() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // A directory where the database should be: it exists, but cannot be opened.
        let db = tmp.path().join("history.db");
        std::fs::create_dir(&db).expect("mkdir");
        let link = HistoryLink::start(db.clone(), tmp.path().into(), "k".into());
        let engine = Engine::start(PortableBackend::default(), EngineOptions::default());
        let mut app = App::new(Theme::no_color(), std::time::Instant::now());
        let mut rec = Recorder::new(Some(link));
        assert!(poll_until(&mut rec, &mut app, &engine));
        let status = app.session.status.clone().expect("a status");
        assert!(
            status.starts_with(&format!("history {}: ", db.display())),
            "{status}"
        );
        assert!(app.session.growth.is_none());
        engine.shutdown(Duration::from_millis(200));
    }

    /// A scan that cannot be recorded is reported in the status line once the writer ends.
    #[test]
    fn record_errors_are_reported() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("scanned");
        std::fs::create_dir(&root).expect("mkdir");
        std::fs::write(root.join("a"), b"data").expect("write");
        // The database's directory is a file: there is no history to load, and none to write.
        let parent = tmp.path().join("state");
        std::fs::write(&parent, b"").expect("write");
        let db = parent.join("history.db");
        let link = HistoryLink::start(db.clone(), root.clone(), "k".into());
        let engine = Engine::start(PortableBackend::default(), EngineOptions::default());
        engine.send(dfdu_engine::Command::Scan {
            path: root.into(),
            opts: dfdu_scan::JobOptions::default(),
        });
        assert!(engine.wait_idle(Some(Duration::from_secs(20))).is_idle());
        let mut app = App::new(Theme::no_color(), std::time::Instant::now());
        let mut rec = Recorder::new(Some(link));
        assert!(poll_until(&mut rec, &mut app, &engine));
        let status = app.session.status.clone().expect("a status");
        assert!(
            status.starts_with(&format!("history {}: ", db.display())),
            "{status}"
        );
        assert!(!rec.busy());
        engine.shutdown(Duration::from_millis(200));
    }
}

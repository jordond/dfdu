//! The run loop returns when the user asks for a scan outside the one shown: `Parent` twice at
//! the scan root, or a folder outside it gone to.

use std::time::Duration;

use crossbeam_channel::unbounded;
use dfdu_engine::{Command, Engine, EngineOptions, Scanned};
use dfdu_scan::{JobOptions, PortableBackend};
use dfdu_tree::HostPath;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::{Terminal, TerminalOptions, Viewport};

use crate::{Exit, UiOptions};

/// Runs the loop on a finished scan of a temporary directory with `keys` and then `q`, and
/// returns how it ended and the directory scanned.
fn run_with(keys: &[KeyEvent]) -> (Exit, HostPath) {
    let (opened, root) = run_opening(keys, |_, path, _| Ok(Ok(path.clone())));
    (opened.map_or(Exit::Quit, Exit::Reopen), root)
}

/// [`run_with`], a scan outside the one shown opened with `open`: what it opened, if anything.
fn run_opening<N>(
    keys: &[KeyEvent],
    open: impl FnMut(
        &mut Terminal<CrosstermBackend<std::io::Sink>>,
        &HostPath,
        Option<Scanned>,
    ) -> std::io::Result<Result<N, String>>,
) -> (Option<N>, HostPath) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let root = dunce::canonicalize(tmp.path())
        .expect("canonical")
        .join("tree");
    std::fs::create_dir_all(root.join("a")).expect("mkdir");
    let engine = Engine::start(
        PortableBackend::default(),
        EngineOptions {
            workers: 1,
            journal_dir: None,
            ..Default::default()
        },
    );
    engine.send(Command::Scan {
        path: root.clone().into(),
        opts: JobOptions::default(),
    });
    let _ = engine.wait_idle(Some(Duration::from_secs(20)));
    let (tx, input) = unbounded();
    for &k in keys {
        tx.send(Event::Key(k)).expect("send");
    }
    let q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
    tx.send(Event::Key(q)).expect("send");
    let viewport = Viewport::Fixed(Rect::new(0, 0, 100, 30));
    let backend = CrosstermBackend::new(std::io::sink());
    let mut terminal =
        Terminal::with_options(backend, TerminalOptions { viewport }).expect("terminal");
    let opened = crate::run_loop::event_loop_opening(
        &mut terminal,
        &engine,
        &input,
        UiOptions::from_env(),
        open,
    )
    .expect("browse");
    engine.shutdown(Duration::from_millis(200));
    (opened, HostPath::local(&root))
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[test]
fn parent_twice_at_the_root_ends_the_loop_with_the_parent() {
    let (exit, root) = run_with(&[key(KeyCode::Left), key(KeyCode::Left)]);
    assert_eq!(exit, Exit::Reopen(root.parent().expect("parent")));
}

#[test]
fn parent_then_another_key_at_the_root_stays() {
    let (exit, _) = run_with(&[key(KeyCode::Left), key(KeyCode::Down), key(KeyCode::Left)]);
    assert_eq!(exit, Exit::Quit);
}

#[test]
fn a_folder_outside_the_scan_ends_the_loop_with_it() {
    let mut keys = vec![
        KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL),
        KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
    ];
    keys.extend("../elsewhere".chars().map(|c| key(KeyCode::Char(c))));
    keys.push(key(KeyCode::Enter));
    let (exit, root) = run_with(&keys);
    let parent = root.parent().expect("parent");
    assert_eq!(exit, Exit::Reopen(parent.join(b"elsewhere")));
}

/// Up past the root, the scan of the parent is opened with what this scan read.
#[test]
fn the_parent_is_opened_with_what_the_scan_read() {
    let up = [key(KeyCode::Left), key(KeyCode::Left)];
    let (opened, root) = run_opening(&up, |_, path, scanned| {
        Ok(Ok((path.clone(), scanned.map(|s| s.root()))))
    });
    let (path, kept) = opened.expect("opened");
    assert_eq!(path, root.parent().expect("parent"));
    assert_eq!(kept, Some(root));
}

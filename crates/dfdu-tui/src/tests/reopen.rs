//! The run loop returns when the user asks for a scan outside the one shown: `Parent` twice at
//! the scan root, or a folder outside it gone to.

use std::time::Duration;

use crossbeam_channel::unbounded;
use dfdu_engine::{Command, Engine, EngineOptions};
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
    let exit = crate::event_loop_with(&mut terminal, &engine, input, UiOptions::from_env())
        .expect("browse");
    engine.shutdown(Duration::from_millis(200));
    (exit, HostPath::local(&root))
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

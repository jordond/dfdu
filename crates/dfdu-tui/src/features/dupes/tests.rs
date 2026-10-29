use std::fs;
use std::time::{Duration, Instant};

use dfdu_engine::{Command as EngineCommand, Engine, EngineOptions};
use dfdu_scan::{JobOptions, PortableBackend};
use ratatui::crossterm::event::KeyCode;

use super::*;
use crate::model::snapshot;
use crate::tests::{press, render};
use crate::theme::Theme;

#[test]
fn duplicates_are_found_marked_and_shown() {
    let tmp = tempfile::tempdir_in(dunce::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
    let dir = tmp.path().to_path_buf();
    fs::create_dir_all(dir.join("photos/old")).unwrap();
    let data: Vec<u8> = (0..(u32::try_from(min_size()).unwrap() + 5000))
        .map(|i| (i % 249) as u8)
        .collect();
    fs::write(dir.join("photos/a.jpg"), &data).unwrap();
    fs::write(dir.join("photos/old/a copy.jpg"), &data).unwrap();
    fs::write(dir.join("b.jpg"), &data).unwrap();
    fs::write(dir.join("other.jpg"), vec![1u8; data.len()]).unwrap();
    let engine = Engine::start(
        PortableBackend::default(),
        EngineOptions {
            workers: 2,
            ..Default::default()
        },
    );
    engine.send(EngineCommand::Scan {
        path: dir.clone(),
        opts: JobOptions::default(),
    });
    assert!(engine.wait_idle(Some(Duration::from_secs(20))).is_idle());
    let t0 = Instant::now();
    let mut app = App::new(Theme::no_color(), t0);
    let mut target = Target::root();
    let snap = engine.read(|m| snapshot::build(m, &mut target, app.size_mode));
    app.target = target;
    app.refresh(snap, t0);

    let fx = app.key(press(KeyCode::Char('4')), t0);
    assert!(matches!(fx[..], [Effect::FindDupes]));
    let (root, cands) = engine.read(|m| {
        let r = m.tree.roots().next().unwrap();
        (
            m.tree.path_buf(r),
            dfdu_dupes::candidates(&m.tree, r, min_size()),
        )
    });
    assert_eq!(cands.len(), 4);
    begin(&mut app, root, cands);
    let deadline = Instant::now() + Duration::from_secs(20);
    while running(&app) {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(stale(&app), "the files need their nodes");
    app.extras.dupes = engine.read(|m| read(m, &app));
    assert!(!stale(&app));
    engine.shutdown(Duration::from_millis(200));

    let out = render(&mut app, t0, 100, 12);
    assert!(out.contains("3 copies of 1.0 MiB"), "{out}");
    assert!(out.contains("photos/old/a copy.jpg"), "{out}");
    assert!(!out.contains("other.jpg"), "{out}");
    // `a` marks every copy but the oldest.
    app.key(press(KeyCode::Char('a')), t0);
    assert_eq!(app.marks.items().len(), 2);
    app.key(press(KeyCode::Char('a')), t0);
    assert_eq!(app.marks.items().len(), 2);
    // Space on the group unmarks its copies again.
    app.dupes.cursor = 0;
    app.key(press(KeyCode::Char(' ')), t0);
    assert!(app.marks.is_empty());
    // A deleted file leaves the group; two left still make one.
    let gone = found(&app).unwrap().groups[0].files[0].path.clone();
    app.extras.dupes.insert(gone, None);
    let out = render(&mut app, t0, 100, 12);
    assert!(out.contains("2 copies of 1.0 MiB"), "{out}");
}

use super::*;
use dfdu_engine::test_support::local_mount;

/// Rows of mounts the scan stayed out of say what they are and how much they use, apart
/// from their size; `m` on one asks the engine to enter it.
#[test]
fn rows_of_mounts_left_out_show_their_use_and_m_enters_them() {
    use ratatui::crossterm::event::KeyCode;

    use crate::app::Effect;
    use crate::model::snapshot::test_entry;
    use crate::tests::{press, render};
    let t0 = std::time::Instant::now();
    let mut app = App::new(crate::theme::Theme::no_color(), t0);
    let row = |i, name: &str, mark| EntryData {
        is_dir: true,
        mark,
        ..test_entry(i, name, 4096)
    };
    let es = vec![
        row(0, "data", Mark::OtherFs),
        row(1, "nas", Mark::Network),
        row(2, "usb", Mark::OtherFs),
        EntryData {
            is_dir: true,
            ..test_entry(3, "src", 1 << 20)
        },
    ];
    app.refresh(crate::tests::snap_with_totals(es, 1), t0);
    let dir = app.snap.dir_path.clone();
    app.outside.set_mount(
        dir.join(b"data"),
        Some(local_mount("zfs", Some(1_300 << 30))),
    );
    let mut nfs = Some(local_mount("nfs4", None));
    if let Some(n) = &mut nfs {
        n.class = dfdu_sys::FsClass::Network;
    }
    app.outside.set_mount(dir.join(b"nas"), nfs);
    let frame = render(&mut app, t0, 140, 10);
    let line = |name: &str| {
        frame
            .lines()
            .find(|l| l.contains(&format!("  {name}/")))
            .unwrap_or_else(|| panic!("{name}:\n{frame}"))
            .to_string()
    };
    assert!(
        line("data").contains("other filesystem (zfs, 1.3 TiB used, not counted): m scans it"),
        "{frame}"
    );
    // The used space is a note, not the row's size.
    assert!(!line("data").contains("1.3 TiB  "), "{frame}");
    assert!(
        line("nas").contains("network filesystem (nfs4): m scans it"),
        "{frame}"
    );
    assert!(
        line("usb").contains("other filesystem: m scans it"),
        "{frame}"
    );
    insta::assert_snapshot!("mounts_left_out", frame);

    // Rows are sorted by size: `src` first, then the mounts by name.
    let at = |app: &App| app.selected_entry().map(|e| e.name.to_vec());
    app.key(press(KeyCode::Char('m')), t0);
    assert!(at(&app).is_some_and(|n| n == b"src"));
    assert!(
        app.status
            .as_deref()
            .is_some_and(|s| s.contains("select one"))
    );
    app.key(press(KeyCode::Down), t0);
    let node = app.selected_entry().expect("selected").node;
    let frame = render(&mut app, t0, 140, 10);
    assert!(frame.contains("m scan it"), "{frame}");
    let fx = app.key(press(KeyCode::Char('m')), t0);
    assert!(
        matches!(fx.as_slice(), [Effect::Send(dfdu_engine::Command::Enter { node: n })] if *n == node),
        "{fx:?}"
    );
}

/// The cleanup view names the rule locations behind links out of the scan.
#[test]
fn the_cleanup_view_lists_links_out_of_the_scan() {
    use ratatui::crossterm::event::KeyCode;

    use crate::features::treemap::tests::{classified, sync};
    use crate::tests::{press, render};
    let fx = classified();
    let t0 = std::time::Instant::now();
    let mut app = App::new(crate::theme::Theme::no_color(), t0);
    app.home = Some("/home/u".into());
    sync(&mut app, &fx.m, t0);
    app.key(press(KeyCode::Char('3')), t0);
    sync(&mut app, &fx.m, t0);
    let rules = fx.m.rules.clone().expect("rules");
    let gradle = rules.index("rust.target").expect("rust.target");
    app.outside.set_linked(vec![Linked {
        link: "/home/u/.gradle".into(),
        target: "/mnt/data/gradle".into(),
        rules: vec![gradle],
    }]);
    let frame = render(&mut app, t0, 120, 24);
    let name = &rules.rule(gradle).name;
    assert!(
        frame.contains(&format!(
            "~/.gradle is a link to /mnt/data/gradle ({name}), not scanned: dfdu /mnt/data/gradle"
        )),
        "{frame}"
    );
    assert!(frame.contains("▸ [ ] OS and user caches"), "{frame}");
}

/// A links lookup whose thread could not be started finds none, so nothing is left pending.
#[test]
fn a_links_lookup_without_a_thread_is_not_pending() {
    let mut o = Outside::default();
    o.look_up_linked(Arc::default(), true);
    assert!(o.pending());
    o.look_up_linked(Arc::default(), false);
    assert!(!o.pending());
    assert!(o.linked().is_empty());
    assert!(o.poll());
}

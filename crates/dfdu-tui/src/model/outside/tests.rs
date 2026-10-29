use super::*;

fn info(fstype: &str, used: Option<u64>) -> MountInfo {
    MountInfo {
        fstype: fstype.into(),
        class: dfdu_sys::FsClass::Local,
        used,
        total: used.map(|u| u * 3),
    }
}

fn at(p: &str, network: bool) -> NotEntered {
    NotEntered {
        path: p.into(),
        network,
    }
}

#[test]
fn footer_lines_name_the_largest_and_count_the_rest() {
    let mut list = vec![
        (at("/mnt/data", false), Some(info("zfs", Some(3 << 40)))),
        (at("/nas", true), Some(info("nfs4", None))),
        (at("/boot", false), Some(info("ext4", Some(300 << 20)))),
    ];
    assert_eq!(
        not_entered_lines(&list, None),
        [
            "not entered: /mnt/data (zfs, 3.0 TiB used), /boot (ext4, 300.0 MiB used); \
             --cross-mounts to include",
            "not entered: /nas (nfs4); --network to include",
        ]
    );
    for i in 0..6 {
        list.push((
            at(&format!("/snap/x/{i}"), false),
            Some(info("squashfs", Some(i))),
        ));
    }
    let lines = not_entered_lines(&list, None);
    assert!(
        lines[0].ends_with(" and 3 more; --cross-mounts to include"),
        "{lines:?}"
    );
    assert!(
        lines[0].starts_with("not entered: /mnt/data (zfs"),
        "{lines:?}"
    );
    // Not looked up (a remote scan): the paths alone.
    let home = Path::new("/home/u");
    let bare = [(at("/home/u/mnt", false), None)];
    assert_eq!(
        not_entered_lines(&bare, Some(home)),
        ["not entered: ~/mnt; --cross-mounts to include"]
    );
}

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
    app.outside
        .set_mount(dir.join(b"data"), Some(info("zfs", Some(1_300 << 30))));
    let mut nfs = Some(info("nfs4", None));
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

#[test]
fn link_notes_name_the_rules_and_where_to_scan() {
    let env = dfdu_rules::Env::with(dfdu_rules::Platform::Linux, &[("HOME", "/home/u")]);
    let (rules, _) = dfdu_rules::builtin(&env);
    let ix = |id: &str| rules.index(id).expect(id);
    let l = Linked {
        link: "/home/u/.cache".into(),
        target: "/mnt/data/cache".into(),
        rules: vec![ix("generic.cachedir-tag")],
    };
    let note = linked_note(&rules, &l, Some(Path::new("/home/u")), "dfdu report");
    let name = &rules.rule(ix("generic.cachedir-tag")).name;
    assert_eq!(
        note,
        format!(
            "~/.cache is a link to /mnt/data/cache ({name}), not scanned: \
             dfdu report /mnt/data/cache"
        )
    );
}

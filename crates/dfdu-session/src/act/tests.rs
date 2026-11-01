use dfdu_engine::UndoInfo;

use super::*;
use crate::snapshot::{EntryData, test_entry};
use crate::state::tests::browser;

/// A browser of `/r` with the directory `lib` (the largest) and the file `f`.
fn lib_and_file() -> SessionState {
    let lib = EntryData {
        is_dir: true,
        ..test_entry(0, "lib", 100)
    };
    browser(vec![lib, test_entry(1, "f", 5)])
}

fn effects(ran: Ran) -> Vec<Effect> {
    match ran {
        Ran::Done(then) => then.effects,
        Ran::FrontEnd => panic!("left to the front end"),
    }
}

#[test]
fn a_sort_key_again_reverses_it() {
    let mut s = lib_and_file();
    sort_by(&mut s, SortKey::Name);
    assert_eq!(s.sort, SortSpec::for_key(SortKey::Name));
    sort_by(&mut s, SortKey::Name);
    assert_eq!((s.sort.key, s.sort.desc), (SortKey::Name, true));
    sort_by(&mut s, SortKey::Size);
    assert_eq!(s.sort, SortSpec::default());
}

#[test]
fn a_size_mode_again_goes_back_to_disk_usage() {
    let mut s = lib_and_file();
    toggle_size(&mut s, SizeMode::Apparent);
    assert_eq!(s.size_mode, SizeMode::Apparent);
    toggle_size(&mut s, SizeMode::Referenced);
    assert_eq!(s.size_mode, SizeMode::Referenced);
    toggle_size(&mut s, SizeMode::Referenced);
    assert_eq!(s.size_mode, SizeMode::Disk);
}

#[test]
fn only_a_mount_left_out_is_scanned_into() {
    let mount = EntryData {
        is_dir: true,
        mark: Mark::OtherFs,
        ..test_entry(0, "nfs", 10)
    };
    let node = mount.node;
    let mut s = browser(vec![mount, test_entry(1, "f", 5)]);
    assert!(matches!(
        enter_mount(&mut s),
        Some(Effect::Send(Command::Enter { node: n })) if n == node
    ));
    assert_eq!(
        s.status.as_deref(),
        Some("scanning nfs/ on its own filesystem…")
    );
    s.move_to(1);
    assert!(enter_mount(&mut s).is_none());
    assert!((s.status.as_deref()).is_some_and(|m| m.starts_with("m scans")));
}

#[test]
fn a_key_of_another_view_says_where_it_works() {
    let mut s = lib_and_file();
    only_in(&mut s, Action::CleanupFilter, &[View::Cleanup]);
    assert_eq!(s.status.as_deref(), Some("f works in Cleanup (3)"));
    only_in(
        &mut s,
        Action::TickSuggested,
        &[View::Cleanup, View::Duplicates],
    );
    assert_eq!(
        s.status.as_deref(),
        Some("A works in Cleanup (3) and Duplicates (4)")
    );
}

#[test]
fn a_read_only_browser_only_says_it_deletes_nothing() {
    let mut s = lib_and_file();
    s.read_only = true;
    for a in [
        Action::Trash,
        Action::DeletePermanent,
        Action::Undo,
        Action::MarkPattern,
    ] {
        s.status = None;
        assert!(effects(run(&mut s, a)).is_empty(), "{a:?}");
        assert_eq!(s.status.as_deref(), Some(READ_ONLY));
        assert!(s.confirm.is_none());
    }
}

#[test]
fn undo_puts_back_the_last_plan_once() {
    let mut s = lib_and_file();
    assert!(effects(run(&mut s, Action::Undo)).is_empty());
    assert_eq!(s.status.as_deref(), Some("nothing to undo"));
    s.snap.undo = Some(UndoInfo {
        plan: 7,
        mode: Mode::Trash,
        items: 2,
        bytes: 1 << 20,
    });
    let fx = effects(run(&mut s, Action::Undo));
    assert!(matches!(fx[..], [Effect::Send(Command::Undo { plan: 7 })]));
    assert_eq!(s.status.as_deref(), Some("putting back 2 items (1.0 MiB)…"));
    // While it runs, not again.
    s.snap.undoing = true;
    assert!(effects(run(&mut s, Action::Undo)).is_empty());
}

#[test]
fn rescan_takes_the_selected_directory_or_the_one_shown() {
    let mut s = lib_and_file();
    let lib = s.snap.entries[0].node;
    let fx = effects(run(&mut s, Action::Rescan));
    assert!(matches!(fx[..], [Effect::Send(Command::Rescan { node })] if node == lib));
    s.move_to(1);
    let dir = s.snap.dir_node.expect("the directory shown");
    let fx = effects(run(&mut s, Action::Rescan));
    assert!(matches!(fx[..], [Effect::Send(Command::Rescan { node })] if node == dir));
    assert_eq!(s.status.as_deref(), Some("rescan requested"));
}

#[test]
fn entering_and_leaving_a_directory_is_left_for_the_scroll() {
    let mut s = lib_and_file();
    let Ran::Done(then) = run(&mut s, Action::Enter) else {
        panic!("the session's");
    };
    let moved = then.entered.expect("went into lib");
    assert_eq!(moved.opened, Some(s.snap.dir_path.join(b"lib")));
    assert!(matches!(run(&mut s, Action::Quit), Ran::FrontEnd));
    assert!(matches!(run(&mut s, Action::Help), Ran::FrontEnd));
}

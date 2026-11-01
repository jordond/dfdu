//! Carrying out the actions of the registry on the list ([`run`]), with those that take more
//! than a line on their own.

use dfdu_engine::{Command, Mode, SizeMode};

use super::{Action, READ_ONLY, first_key};
use crate::app::views::{self, View};
use crate::app::{App, Effect, Overlay};
use crate::features::{cleanup, debug, details, dupes, review, selected_path, treemap};
use crate::model::snapshot::Mark;
use crate::overlay::{help, mark_pattern, palette, type_filter};
use crate::state::order::{SortKey, SortSpec};
use crate::state::{filter, marks, tree};

/// Carries out `a` on the list (no overlay open) and returns the effects for the run loop. In a
/// read-only browser an action that deletes only says so.
pub fn run(app: &mut App, a: Action) -> Vec<Effect> {
    let mut fx = Vec::new();
    if app.session.read_only && a.deletes() {
        app.session.status = Some(READ_ONLY.into());
        return fx;
    }
    let page = isize::try_from(app.list_height.max(1)).unwrap_or(isize::MAX);
    let before = app.session.selected_index();
    match a {
        Action::Quit => app.quit = true,
        Action::Up => app.session.move_by(-1),
        Action::Down => app.session.move_by(1),
        Action::PageUp => app.session.move_by(-page),
        Action::PageDown => app.session.move_by(page),
        Action::HalfPageUp => app.session.move_by(-(page / 2).max(1)),
        Action::HalfPageDown => app.session.move_by((page / 2).max(1)),
        Action::First => app.session.move_to(0),
        Action::Last => app
            .session
            .move_to(app.session.list.rows.len().saturating_sub(1)),
        Action::Open if tree::act::right(app) => {}
        Action::Open | Action::Enter => {
            let dir = app
                .session
                .selected_index()
                .map(|row| app.session.list.rows[row]);
            if let Some(i) = dir.filter(|&i| app.session.snap.entries[i].is_dir) {
                app.enter(i);
            }
        }
        Action::Parent if tree::act::left(app) => {}
        Action::Parent => app.up(),
        Action::SortSize => sort_by(app, SortKey::Size),
        Action::SortName => sort_by(app, SortKey::Name),
        Action::SortItems => sort_by(app, SortKey::Items),
        Action::SortMtime => sort_by(app, SortKey::Mtime),
        Action::Resort => app.session.request_sort(),
        Action::ToggleApparent => toggle_size(app, SizeMode::Apparent),
        Action::ToggleReferenced => toggle_size(app, SizeMode::Referenced),
        Action::ToggleHidden => {
            app.session.hide_hidden = !app.session.hide_hidden;
            app.session.request_sort();
        }
        Action::Columns => {
            app.columns = app.columns.next();
            app.session.status = Some(app.columns.describe().into());
        }
        Action::ToggleTree => tree::act::toggle(app),
        Action::ExpandChain => tree::act::chain(app),
        Action::CycleFold => tree::act::cycle_fold(app),
        Action::Rescan => {
            let node = match app.session.selected_entry() {
                Some(e) if e.is_dir => Some(e.node),
                _ => app.session.snap.dir_node,
            };
            if let Some(node) = node {
                fx.push(Effect::Send(Command::Rescan { node }));
                app.session.status = Some("rescan requested".into());
            }
        }
        Action::EnterMount => fx.extend(enter_mount(app)),
        Action::SkipStalled => {
            if app.session.snap.stalled > 0 {
                fx.push(Effect::Send(Command::AbandonAllStalled));
                app.session.status = Some("skipping directories that are not responding".into());
            }
        }
        Action::Trash => fx.extend(review::delete(app, Mode::Trash)),
        Action::DeletePermanent => fx.extend(review::delete(app, Mode::Permanent)),
        Action::Undo => match &app.session.snap.undo {
            Some(u) if !app.session.snap.undoing => {
                fx.push(Effect::Send(Command::Undo { plan: u.plan }));
                app.session.status = Some(format!(
                    "putting back {} item{} ({})…",
                    u.items,
                    crate::format::plural(u.items as u64),
                    crate::format::human(u.bytes)
                ));
            }
            _ => app.session.status = Some("nothing to undo".into()),
        },
        Action::Help => help::open(app),
        Action::Errors => fx.push(Effect::ShowErrors),
        Action::Close => {
            if !marks::range::end(app) && !filter::clear(app) && !type_filter::clear(app) {
                app.overlay = Overlay::None;
                if views::current(app) != View::Files {
                    fx.extend(views::switch(app, View::Files));
                }
            }
        }
        Action::ToggleMark => marks::toggle(app),
        Action::ClearMarks => marks::clear(app),
        Action::RangeMark => marks::range::toggle(app),
        Action::MarkPattern => mark_pattern::open(app),
        Action::InvertMarks => marks::invert(app),
        Action::CopyPath => fx.extend(selected_path::copy(app)),
        Action::OpenPath => fx.extend(selected_path::open(app)),
        Action::Filter => filter::open(app),
        Action::Types => type_filter::open(app),
        Action::Details => details::toggle(app),
        Action::FocusDetails => fx.extend(details::focus(app)),
        Action::Palette => palette::open(app),
        Action::Debug => debug::toggle(app),
        Action::ViewFiles => fx.extend(views::switch(app, View::Files)),
        Action::ViewMap => fx.extend(views::switch(app, View::Map)),
        Action::ViewCleanup => fx.extend(views::switch(app, View::Cleanup)),
        Action::ViewDupes => fx.extend(views::switch(app, View::Duplicates)),
        Action::ViewLargest => fx.extend(views::switch(app, View::Largest)),
        Action::PrevView => fx.extend(views::step(app, -1)),
        Action::NextView => fx.extend(views::step(app, 1)),
        Action::DupesKeep => crate::features::dupes::cycle_keep(app),
        Action::TickSuggested => match views::current(app) {
            View::Cleanup => cleanup::tick_suggested(app),
            View::Duplicates => dupes::tick_suggested(app),
            _ => only_in(app, a, &[View::Cleanup, View::Duplicates]),
        },
        Action::CompareAgain if dupes::active(app) => fx.extend(dupes::compare_again(app)),
        Action::CleanupFilter if cleanup::active(app) => cleanup::cycle_filter(app),
        Action::CleanupCommand if cleanup::active(app) => fx.extend(cleanup::command(app)),
        Action::CompareAgain => only_in(app, a, &[View::Duplicates]),
        Action::CleanupFilter | Action::CleanupCommand => only_in(app, a, &[View::Cleanup]),
        Action::TreemapDeeper => treemap::deeper(app, 1),
        Action::TreemapShallower => treemap::deeper(app, -1),
        Action::TreemapColor => treemap::cycle_color(app),
    }
    if a.moves() {
        marks::range::follow(app, before);
    }
    fx
}

/// A sort key: the key's default direction, or the other direction if it is already in use.
fn sort_by(app: &mut App, key: SortKey) {
    app.session.sort = if app.session.sort.key == key {
        SortSpec {
            key,
            desc: !app.session.sort.desc,
        }
    } else {
        SortSpec::for_key(key)
    };
    app.session.request_sort();
}

/// Asks the engine to scan into the selected mount the scan stayed out of.
fn enter_mount(app: &mut App) -> Option<Effect> {
    match app.session.selected_entry() {
        Some(e) if matches!(e.mark, Mark::OtherFs | Mark::Network) => {
            let name = String::from_utf8_lossy(&e.name).into_owned();
            let node = e.node;
            app.session.status = Some(format!("scanning {name}/ on its own filesystem…"));
            Some(Effect::Send(Command::Enter { node }))
        }
        _ => {
            app.session.status =
                Some("m scans a mount the scan stayed out of; select one first".into());
            None
        }
    }
}

/// The status line after the key of `a`, which acts only in `views`, in another view.
fn only_in(app: &mut App, a: Action, views: &[View]) {
    let names: Vec<String> = (views.iter())
        .map(|v| format!("{} ({})", v.name(), first_key(v.action())))
        .collect();
    app.session.status = Some(format!("{} works in {}", first_key(a), names.join(" and ")));
}

/// Switches to `mode`, or back to disk usage if `mode` is shown already.
fn toggle_size(app: &mut App, mode: SizeMode) {
    app.session.size_mode = if app.session.size_mode == mode {
        SizeMode::Disk
    } else {
        mode
    };
    app.session.request_sort();
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use dfdu_engine::UndoInfo;

    use super::*;
    use crate::model::snapshot::{EntryData, test_entry};
    use crate::tests::{library_snap, snap_with_totals};
    use crate::theme::Theme;

    fn browser() -> (App, Instant) {
        let t0 = Instant::now();
        let mut app = App::new(Theme::no_color(), t0);
        app.refresh(library_snap(), t0);
        (app, t0)
    }

    #[test]
    fn a_sort_key_again_reverses_it() {
        let (mut app, _) = browser();
        sort_by(&mut app, SortKey::Name);
        assert_eq!(app.session.sort, SortSpec::for_key(SortKey::Name));
        sort_by(&mut app, SortKey::Name);
        assert_eq!(
            (app.session.sort.key, app.session.sort.desc),
            (SortKey::Name, true)
        );
        sort_by(&mut app, SortKey::Size);
        assert_eq!(app.session.sort, SortSpec::default());
    }

    #[test]
    fn a_size_mode_again_goes_back_to_disk_usage() {
        let (mut app, _) = browser();
        toggle_size(&mut app, SizeMode::Apparent);
        assert_eq!(app.session.size_mode, SizeMode::Apparent);
        toggle_size(&mut app, SizeMode::Referenced);
        assert_eq!(app.session.size_mode, SizeMode::Referenced);
        toggle_size(&mut app, SizeMode::Referenced);
        assert_eq!(app.session.size_mode, SizeMode::Disk);
    }

    #[test]
    fn only_a_mount_left_out_is_scanned_into() {
        let t0 = Instant::now();
        let mut app = App::new(Theme::no_color(), t0);
        let mount = EntryData {
            is_dir: true,
            mark: Mark::OtherFs,
            ..test_entry(0, "nfs", 10)
        };
        let node = mount.node;
        app.refresh(snap_with_totals(vec![mount, test_entry(1, "f", 5)], 1), t0);
        assert!(matches!(
            enter_mount(&mut app),
            Some(Effect::Send(Command::Enter { node: n })) if n == node
        ));
        assert_eq!(
            app.session.status.as_deref(),
            Some("scanning nfs/ on its own filesystem…")
        );
        app.session.move_to(1);
        assert!(enter_mount(&mut app).is_none());
        assert!(
            app.session
                .status
                .as_deref()
                .is_some_and(|s| s.starts_with("m scans"))
        );
    }

    #[test]
    fn a_key_of_another_view_says_where_it_works() {
        let (mut app, _) = browser();
        only_in(&mut app, Action::CleanupFilter, &[View::Cleanup]);
        assert_eq!(
            app.session.status.as_deref(),
            Some("f works in Cleanup (3)")
        );
        only_in(
            &mut app,
            Action::TickSuggested,
            &[View::Cleanup, View::Duplicates],
        );
        assert_eq!(
            app.session.status.as_deref(),
            Some("A works in Cleanup (3) and Duplicates (4)")
        );
    }

    #[test]
    fn a_read_only_browser_only_says_it_deletes_nothing() {
        let (mut app, _) = browser();
        app.session.read_only = true;
        for a in [Action::Trash, Action::DeletePermanent, Action::Undo] {
            app.session.status = None;
            assert!(run(&mut app, a).is_empty(), "{a:?}");
            assert_eq!(app.session.status.as_deref(), Some(READ_ONLY));
            assert!(matches!(app.overlay, Overlay::None));
        }
    }

    #[test]
    fn undo_puts_back_the_last_plan_once() {
        let (mut app, _) = browser();
        assert!(run(&mut app, Action::Undo).is_empty());
        assert_eq!(app.session.status.as_deref(), Some("nothing to undo"));
        app.session.snap.undo = Some(UndoInfo {
            plan: 7,
            mode: Mode::Trash,
            items: 2,
            bytes: 1 << 20,
        });
        let fx = run(&mut app, Action::Undo);
        assert!(matches!(fx[..], [Effect::Send(Command::Undo { plan: 7 })]));
        assert_eq!(
            app.session.status.as_deref(),
            Some("putting back 2 items (1.0 MiB)…")
        );
        // While it runs, not again.
        app.session.snap.undoing = true;
        assert!(run(&mut app, Action::Undo).is_empty());
    }

    #[test]
    fn rescan_takes_the_selected_directory_or_the_one_shown() {
        let (mut app, _) = browser();
        let library = app.session.snap.entries[0].node;
        let fx = run(&mut app, Action::Rescan);
        assert!(matches!(fx[..], [Effect::Send(Command::Rescan { node })] if node == library));
        app.session.move_to(1);
        let dir = app.session.snap.dir_node.expect("the directory shown");
        let fx = run(&mut app, Action::Rescan);
        assert!(matches!(fx[..], [Effect::Send(Command::Rescan { node })] if node == dir));
        assert_eq!(app.session.status.as_deref(), Some("rescan requested"));
    }
}

//! Clicks on the header: a breadcrumb goes to its directory, a tab to its view.

use super::*;
use crate::model::snapshot::Snapshot;

/// A drawn browser of `/r/a/b`, below the scan root `/r`.
fn below_root(t0: Instant) -> App {
    let mut app = App::new(Theme::no_color(), t0);
    let below = Snapshot {
        dir_path: "/r/a/b".into(),
        root_path: "/r".into(),
        at_root: false,
        ..snap_without_totals(vec![dir(0, "c", 10)], 1)
    };
    app.refresh(below, t0);
    render(&mut app, t0, 80, 12);
    app
}

#[test]
fn click_on_breadcrumb_goes_up() {
    let t0 = Instant::now();
    let mut app = below_root(t0);
    let crumbs: Vec<_> = app.mouse.crumbs.iter().map(|c| c.area).collect();
    // The scope line " /r › a › b": `/r` at columns 1-2, `a` at 6.
    assert_eq!(crumbs, [Rect::new(1, 1, 2, 1), Rect::new(6, 1, 1, 1)]);
    // The current directory and the separators are not targets.
    assert!(event(&mut app, left(10, 1), t0).is_none());
    assert!(event(&mut app, left(4, 1), t0).is_none());
    event(&mut app, left(6, 1), t0).expect("redraw");
    assert_eq!(app.session.target.path, HostPath::from("/r").join(b"a"));
    assert_eq!(app.session.selected.as_ref().expect("selection").name, b"b");

    let mut app = below_root(t0);
    event(&mut app, left(2, 1), t0).expect("redraw");
    assert_eq!(app.session.target.path, HostPath::from("/r"));
    assert_eq!(app.session.selected.as_ref().expect("selection").name, b"a");
}

#[test]
fn click_on_a_tab_switches_to_its_view() {
    use crate::app::views::{View, current};
    let t0 = Instant::now();
    let mut app = browser(t0);
    let tab = |app: &App, view| {
        let t = app.mouse.tabs.iter().find(|t| t.view == view);
        t.expect("the tab is drawn").area
    };
    assert_eq!(app.mouse.tabs.len(), 5);
    let largest = tab(&app, View::Largest);
    assert_eq!(largest.y, 0);
    event(&mut app, left(largest.right() - 1, 0), t0).expect("redraw");
    assert_eq!(current(&app), View::Largest);
    render(&mut app, t0, 80, 12);
    let files = tab(&app, View::Files);
    event(&mut app, left(files.x, 0), t0).expect("redraw");
    assert_eq!(current(&app), View::Files);
    // The title and the scan state are not targets.
    assert!(event(&mut app, left(2, 0), t0).is_none());
    assert!(event(&mut app, left(79, 0), t0).is_none());
}

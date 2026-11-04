//! Which screen follows a scan asked for from a browser, as logic: no window is drawn.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use dfdu_session::SessionState;

use super::*;
use crate::browser::ViewState;
use crate::theme::Colors;

/// A browser that only keeps its state and says when it was stopped.
struct Fake {
    state: SessionState,
    view: ViewState,
    stopped: Arc<AtomicBool>,
}

impl Browse for Fake {
    fn show(&mut self, _: &mut egui::Ui, _: &Colors) {}

    fn state(&self) -> &SessionState {
        &self.state
    }

    fn state_mut(&mut self) -> &mut SessionState {
        &mut self.state
    }

    fn view_mut(&mut self) -> &mut ViewState {
        &mut self.view
    }

    fn shutdown(self: Box<Self>) {
        self.stopped.store(true, Ordering::SeqCst);
    }
}

/// A browser, and whether it has been stopped.
fn fake() -> (Box<dyn Browse>, Arc<AtomicBool>) {
    let stopped = Arc::new(AtomicBool::new(false));
    let b = Fake {
        state: SessionState::new(Instant::now()),
        view: ViewState::default(),
        stopped: stopped.clone(),
    };
    (Box::new(b), stopped)
}

fn target(path: &str) -> Target {
    Target {
        host: "nas".into(),
        port: Some(2222),
        identity: None,
        path: path.into(),
    }
}

fn start() -> Back {
    Back::Start(Start::new(&egui::Context::default()), launch::local(None))
}

#[test]
fn a_scan_is_opened_the_way_the_browser_s_own_was() {
    let path = HostPath::local(std::path::Path::new("/mnt/disk1"));
    assert_eq!(plan(&Origin::Local, &path), Plan::Local(path.to_local()));
    assert_eq!(plan(&Origin::Admin, &path), Plan::Admin(path.to_local()));
    let remote = Origin::Remote(target("/mnt/cache"));
    assert_eq!(
        plan(&remote, &HostPath::from("/mnt/disk1")),
        Plan::Connect(target("/mnt/disk1")),
        "the same computer, port and key, another folder"
    );
    assert_eq!(plan(&Origin::Imported, &path), Plan::FromFile);
}

#[test]
fn the_new_browser_replaces_the_old_one_only_once_it_opened() {
    let (mut old, old_stopped) = fake();
    old.state_mut().snap.root_path = HostPath::from("/mnt/cache");
    let (new, new_stopped) = fake();
    let screen = replaced(Ok(new), Origin::Local, Some((old, Origin::Admin)));
    let Screen::Browse(b, Origin::Local) = &screen else {
        panic!("not the new browser");
    };
    assert_eq!(
        b.state().came_from,
        Some(HostPath::from("/mnt/cache")),
        "the new browser selects the folder come from"
    );
    assert!(old_stopped.load(Ordering::SeqCst) && !new_stopped.load(Ordering::SeqCst));

    let (old, old_stopped) = fake();
    let failed = Err(io::Error::other("no such folder"));
    let Screen::Browse(b, origin) = replaced(failed, Origin::Local, Some((old, Origin::Admin)))
    else {
        panic!("not the old browser");
    };
    assert_eq!(origin, Origin::Admin, "the old browser keeps its origin");
    assert_eq!(
        b.state().status.as_deref(),
        Some("cannot open it: no such folder")
    );
    assert!(!old_stopped.load(Ordering::SeqCst));

    let failed = Err(io::Error::other("no such folder"));
    assert!(matches!(
        replaced(failed, Origin::Local, None),
        Screen::Failed(_)
    ));
}

#[test]
fn an_administrator_s_scan_that_did_not_start_goes_back() {
    let (b, stopped) = fake();
    let back = Some(Back::Browse(b, Origin::Admin));
    let Screen::Browse(b, Origin::Admin) = not_elevated(back, Some("refused".into())) else {
        panic!("not the browser");
    };
    assert_eq!(b.state().status.as_deref(), Some("refused"));
    assert!(!stopped.load(Ordering::SeqCst));

    // Cancelled: the browser as it was, the start screen, or nothing.
    let (b, _) = fake();
    let screen = not_elevated(Some(Back::Browse(b, Origin::Admin)), None);
    assert!(matches!(&screen, Screen::Browse(b, _) if b.state().status.is_none()));
    assert!(matches!(
        not_elevated(Some(start()), None),
        Screen::Start { .. }
    ));
    assert!(matches!(not_elevated(None, None), Screen::Closed));
    assert!(matches!(
        not_elevated(None, Some("x".into())),
        Screen::Failed(_)
    ));
}

#[test]
fn a_connection_that_failed_goes_back_with_why() {
    let (b, stopped) = fake();
    let back = Back::Browse(b, Origin::Remote(target("/mnt/cache")));
    let Screen::Browse(b, _) = not_connected(back, &target("/mnt/disk1"), "timed out".into())
    else {
        panic!("not the browser");
    };
    assert_eq!(
        b.state().status.as_deref(),
        Some("cannot scan /mnt/disk1: timed out")
    );
    assert!(!stopped.load(Ordering::SeqCst));

    // Cancelled: nothing to say.
    let (b, _) = fake();
    let back = Back::Browse(b, Origin::Local);
    let screen = not_connected(back, &target("/x"), String::new());
    assert!(matches!(&screen, Screen::Browse(b, _) if b.state().status.is_none()));

    let screen = not_connected(start(), &target("/x"), "timed out".into());
    let Screen::Start { start, .. } = screen else {
        panic!("not the start screen");
    };
    assert_eq!(start.remote_error.as_deref(), Some("timed out"));
}

#[test]
fn only_a_computer_chosen_on_the_start_screen_is_remembered() {
    assert!(start().remembers());
    let (b, _) = fake();
    assert!(!Back::Browse(b, Origin::Remote(target("/mnt/cache"))).remembers());
}

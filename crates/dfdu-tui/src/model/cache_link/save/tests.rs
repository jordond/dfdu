use std::time::{Duration, Instant};

use dfdu_engine::Engine;

use super::super::SavedAs;
use super::super::tests::{cacher, link, poll_until};
use crate::app::App;
use crate::theme::Theme;

/// A save whose thread cannot start is reported in the status line, and the tree is not tried
/// again until it changes.
#[test]
fn a_save_that_cannot_start_is_reported_and_not_retried() {
    let tmp = tempfile::tempdir().unwrap();
    let link = link(tmp.path());
    let e = Engine::portable(2);
    let mut app = App::new(Theme::no_color(), Instant::now());
    let mut c = cacher(&link);
    poll_until(&mut c, &mut app, &e, |c, _| {
        c.run
            .as_ref()
            .is_some_and(|r| r.saved_as != SavedAs::Pending)
            && c.writer.is_none()
    });
    let version = c.saved.expect("saved once");
    c.saved = None;
    let failed = Err(std::io::Error::other("no threads"));
    assert!(c.save_started(&link, &mut app, version, failed));
    let status = app.session.status.clone().expect("a notice");
    assert_eq!(
        status,
        format!(
            "cache {}: cannot start a thread: no threads",
            link.slot.path.display()
        )
    );
    assert_eq!(c.saved, Some(version));
    for _ in 0..3 {
        assert!(!c.poll(&mut app, &e));
    }
    assert!(c.writer.is_none() && c.changed.is_none());
    assert!(!c.busy());
    e.shutdown(Duration::from_millis(200));
}

use std::time::Instant;

use dfdu_tree::Safety;

use super::*;
use crate::cleanup::{Intent as ViewIntent, Row, act as view_act, rows};
use crate::state::tests::sync;
use crate::test_support::classified;
use crate::view::View;

/// A browser of [`classified`] in the cleanup view, read-only or not, with the item of the
/// regenerable group selected: `big`, a Rust `target/` cleaned with `cargo clean`.
fn on_the_rust_target(read_only: bool) -> (SessionState, crate::test_support::Fixture) {
    let fx = classified();
    let mut s = SessionState::new(Instant::now());
    s.read_only = read_only;
    sync(&mut s, &fx.m);
    s.view = View::Cleanup;
    let _ = crate::cleanup::on_open(&mut s, false);
    sync(&mut s, &fx.m);
    let at = rows(&s)
        .iter()
        .position(|r| {
            matches!(
                r,
                Row::Group {
                    safety: Safety::Regenerable,
                    ..
                }
            )
        })
        .expect("the regenerable group");
    s.cleanup.cursor = at;
    let _ = view_act(&mut s, ViewIntent::Activate);
    s.cleanup.cursor = at + 1;
    (s, fx)
}

/// The output of a finished run of the pane's command, as a read brings it in.
fn finished(s: &mut SessionState) {
    let id = s.cleanup.pane.as_ref().expect("the pane is open").id;
    s.extras.cleanup.native = Some(NativeView {
        id,
        lines: vec!["Summary 3 files".into()],
        done: Some(Ok(())),
    });
}

fn runs(fx: &[Effect]) -> Option<(bool, Option<usize>)> {
    match fx {
        [
            Effect::Send(Command::RunNative {
                dry_run, variant, ..
            }),
        ] => Some((*dry_run, *variant)),
        [] => None,
        other => panic!("more than one run: {other:?}"),
    }
}

#[test]
fn a_native_command_starts_with_its_dry_run() {
    let (mut s, _fx) = on_the_rust_target(false);
    assert_eq!(runs(&open(&mut s)), Some((true, None)));
    // While it runs, nothing else happens and the pane stays.
    for intent in [Intent::Run, Intent::Close, Intent::OtherForm] {
        assert!(act(&mut s, intent).is_empty(), "{intent:?}");
    }
    assert!(s.cleanup.pane.is_some());
    finished(&mut s);
    assert_eq!(runs(&act(&mut s, Intent::Run)), Some((false, None)));
    finished(&mut s);
    assert!(act(&mut s, Intent::Close).is_empty());
    assert!(s.cleanup.pane.is_none());
}

/// An imported scan or `--read-only` runs a native command's dry run, but not the command.
#[test]
fn a_read_only_browser_runs_only_the_dry_run() {
    let (mut s, _fx) = on_the_rust_target(true);
    assert_eq!(runs(&open(&mut s)), Some((true, None)));
    finished(&mut s);
    assert!(act(&mut s, Intent::Run).is_empty());
    assert_eq!(s.status.as_deref(), Some(READ_ONLY));
    assert!(s.cleanup.pane.is_some());
}

#[test]
fn a_match_without_a_native_command_says_so() {
    let (mut s, _fx) = on_the_rust_target(false);
    // The safe group's item, a directory with a `CACHEDIR.TAG`, has none.
    let at = rows(&s)
        .iter()
        .position(|r| {
            matches!(
                r,
                Row::Group {
                    safety: Safety::Safe,
                    ..
                }
            )
        })
        .expect("the safe group");
    s.cleanup.cursor = at;
    let _ = view_act(&mut s, ViewIntent::Activate);
    s.cleanup.cursor = at + 1;
    assert!(open(&mut s).is_empty());
    assert!(s.cleanup.pane.is_none());
    assert_eq!(
        s.status.as_deref(),
        Some("no native command for this item; d or D deletes it")
    );
}

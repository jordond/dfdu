//! The Cleanup view: the suggested items are ticked on the first opening and a protected one
//! never is (gate 6); the levels shown, the ticks and the details of a match; and the native
//! command pane, which runs a command's dry run first, runs a further-reaching form only when
//! asked twice and cannot be closed while its command runs, and in a read-only browser runs
//! only the dry run (gate 7). The rules are written for the tests; their native commands are
//! `ls`, `sleep` and `true`.

use std::path::Path;
use std::process::{Command, Stdio};

use dfdu_session::cleanup::{self, Row};
#[cfg(unix)]
use dfdu_session::state::READ_ONLY;
use dfdu_session::view::View;
use dfdu_tree::Safety;
use egui::Key;
use egui_kittest::Harness;
#[cfg(unix)]
use egui_kittest::kittest::NodeT;
use egui_kittest::kittest::Queryable;

#[cfg(unix)]
use super::settle;
use super::{Window, age, close, harness, scan_done, shown, shown_part, state, until};

/// `cache` directories are safe to delete; `build` directories are regenerable, unless git
/// tracks them, with a native command whose dry run lists them and whose further-reaching form
/// takes a second to dry-run. The commands themselves change nothing.
const RULES: &str = r#"schema_version = 1
[[rule]]
id = "test.cache"
category = "sys.cache"
name = "Test cache"
safety = "safe"
action = "delete"
explanation = "A cache for the tests."
lost = "Nothing the tests need."
[rule.match]
kind = "dir"
names = ["cache"]
stop_descent = true

[[rule]]
id = "test.build"
category = "dev.build"
name = "Test build output"
safety = "regenerable"
action = "native"
explanation = "Build output for the tests."
lost = "Nothing the tests need."
[rule.match]
kind = "dir"
names = ["build"]
stop_descent = true
[rule.guard]
not_vcs_tracked = true
[rule.native]
check = ["true"]
dry_run = ["ls", "{path}"]
command = ["true"]
cwd = "{parent}"
[[rule.native.variant]]
name = "a slower form"
dry_run = ["sleep", "1"]
command = ["true"]
"#;

fn write(p: &Path, bytes: usize) {
    std::fs::create_dir_all(p.parent().expect("a parent")).expect("mkdir");
    std::fs::write(p, vec![1u8; bytes]).expect("write");
}

/// Runs git in `dir`; false when git is not there or fails.
fn git(dir: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// A window on the Cleanup view of `dir`, once the scan is done and the view lists `n` matches;
/// what it runs is journalled in `journal`.
fn opened(dir: &Path, read_only: bool, journal: &Path, n: usize) -> Harness<'static, Window> {
    let launch = super::with_rules(dir, read_only, Some(journal.join("ops")), RULES);
    let mut h = harness(launch);
    until(&mut h, "the scan", scan_done);
    h.event(egui::Event::Text("3".into()));
    until(&mut h, "the matches", |h| {
        let s = state(h);
        s.current_view() == View::Cleanup && s.extras.cleanup.items.len() == n
    });
    h
}

/// The row of the view that `pick` finds.
fn row_of(h: &Harness<Window>, pick: impl Fn(&Row) -> bool) -> usize {
    cleanup::rows(state(h))
        .iter()
        .position(pick)
        .expect("the row")
}

/// Moves the cursor to row `to` with the arrows.
fn go_to(h: &mut Harness<'static, Window>, to: usize) {
    h.key_press(Key::Home);
    for _ in 0..to {
        h.key_press(Key::ArrowDown);
    }
    h.step();
    assert_eq!(state(h).cleanup.cursor, to);
}

/// Whether the item whose path ends with `tail` is ticked.
fn ticked(h: &Harness<Window>, tail: &[&str]) -> bool {
    let s = state(h);
    let it = (s.extras.cleanup.items.iter())
        .find(|it| {
            it.path
                .to_local()
                .ends_with(tail.iter().collect::<std::path::PathBuf>())
        })
        .expect("the item");
    cleanup::is_marked(s, it)
}

#[test]
fn cleanup_ticks_the_suggested_items_on_its_first_opening_and_never_a_protected_one() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let root = tmp.path();
    write(&root.join("old").join("cache").join("blob"), 8 * 1024);
    write(&root.join("new").join("cache").join("blob"), 4 * 1024);
    // Unchanged for 30 days: suggested. Changed now: too recent.
    age(&root.join("old").join("cache"), 30);
    // A build directory git tracks is protected, when git is there to tell.
    let repo = root.join("repo");
    write(&repo.join("build").join("out.o"), 2 * 1024);
    let tracked = git(&repo, &["init", "-q"]) && git(&repo, &["add", "build"]);
    let journal = tempfile::tempdir().expect("temp dir");
    let mut h = opened(root, false, journal.path(), 3);

    // The first opening ticked the old cache, and said so.
    until(&mut h, "the suggestion", |h| {
        shown_part(h, "Ticked 1 suggested item")
    });
    assert!(ticked(&h, &["old", "cache"]));
    assert!(!ticked(&h, &["new", "cache"]));
    assert!(shown(&h, "Safe to delete") && shown_part(&h, "1 ticked ("));

    // The details of a match: its rule, what is lost, and why it is suggested or not.
    let cache = row_of(&h, |r| {
        matches!(
            r,
            Row::Group {
                safety: Safety::Safe,
                ..
            }
        )
    });
    go_to(&mut h, cache);
    h.key_press(Key::Enter);
    until(&mut h, "the category's items", |h| {
        cleanup::rows(state(h)).len() > 4
    });
    h.key_press(Key::ArrowDown);
    until(&mut h, "the details", |h| {
        shown(h, "Test cache") && shown(h, "You lose: Nothing the tests need.")
    });
    assert!(cleanup::selected(state(&h)).is_some());
    assert!(
        shown(&h, "Suggested: safe to delete.")
            || shown(&h, "Not suggested: changed in the last 7 days.")
    );

    // `f` shows only the safer levels, one fewer each time, then everything again.
    h.event(egui::Event::Text("f".into()));
    until(&mut h, "safe only", |h| {
        shown(h, "cleanup: showing safe only")
            && shown(h, "Safe only")
            && !cleanup::rows(state(h)).iter().any(|r| match r {
                Row::Section(s) => *s > Safety::Safe,
                _ => false,
            })
    });
    // A click on a level shows it at once.
    h.get_by_label("Up to review").click();
    until(&mut h, "up to review", |h| {
        state(h).cleanup.filter == Some(Safety::Review)
    });
    h.event(egui::Event::Text("f".into()));
    until(&mut h, "everything", |h| {
        shown(h, "cleanup: showing everything") && state(h).cleanup.filter.is_none()
    });

    // Unticked, the suggestion ticks it again; its tick unticks it.
    h.event(egui::Event::Text("X".into()));
    until(&mut h, "no ticks", |h| state(h).marks.is_empty());
    h.get_by_label("Tick suggested").click();
    until(&mut h, "the suggestion again", |h| {
        ticked(h, &["old", "cache"]) && !ticked(h, &["new", "cache"])
    });

    if tracked {
        // Once git says so, the tracked build directory is protected: listed, never ticked.
        until(&mut h, "the protected match", |h| {
            cleanup::rows(state(h)).contains(&Row::Section(Safety::Protected))
        });
        assert!(shown(&h, "Protected"));
        let protected = row_of(&h, |r| {
            matches!(
                r,
                Row::Group {
                    safety: Safety::Protected,
                    ..
                }
            )
        });
        let before = state(&h).marks.items().len();
        go_to(&mut h, protected);
        h.key_press(Key::Space);
        until(&mut h, "the refusal", |h| {
            shown(h, "protected: cannot be marked")
        });
        assert_eq!(state(&h).marks.items().len(), before);
        assert!(!ticked(&h, &["repo", "build"]));
    }
    close(h);
}

/// A directory with a `build` directory holding `a.o`.
#[cfg(unix)]
fn project() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("temp dir");
    write(&tmp.path().join("proj").join("build").join("a.o"), 4 * 1024);
    tmp
}

/// Selects the one build directory's row.
#[cfg(unix)]
fn select_build(h: &mut Harness<'static, Window>) {
    let group = row_of(h, |r| matches!(r, Row::Group { .. }));
    go_to(h, group);
    h.key_press(Key::Enter);
    until(h, "the item", |h| cleanup::rows(state(h)).len() == 3);
    h.key_press(Key::ArrowDown);
    h.step();
    assert!(cleanup::selected(state(h)).is_some());
}

/// Whether the button named `label` is disabled.
#[cfg(unix)]
fn disabled(h: &Harness<Window>, label: &str) -> bool {
    h.get_by_label(label).accesskit_node().is_disabled()
}

#[cfg(unix)]
#[test]
fn a_native_command_runs_its_dry_run_first_and_a_further_form_when_asked_twice() {
    let tmp = project();
    let journal = tempfile::tempdir().expect("temp dir");
    let mut h = opened(tmp.path(), false, journal.path(), 1);
    select_build(&mut h);

    // `N` opens the pane on the dry run, whose output it shows.
    h.event(egui::Event::Text("N".into()));
    until(&mut h, "the dry run", |h| {
        shown(h, "That was the dry run.") && shown(h, "a.o")
    });
    settle(&mut h);
    assert!(!disabled(&h, "Run it"));

    // Another form: its dry run takes a second, and while it runs nothing closes the pane.
    h.event(egui::Event::Text("v".into()));
    until(&mut h, "the other form's dry run", |h| shown(h, "running…"));
    assert!(disabled(&h, "Close") && disabled(&h, "Run it"));
    h.key_press(Key::Escape);
    h.step();
    assert!(state(&h).cleanup.pane.is_some(), "the pane stays open");
    until(&mut h, "the other form's dry run to finish", |h| {
        shown(h, "That was the dry run.")
    });
    settle(&mut h);

    // It removes more than the default: the first ask only warns, the second runs it.
    h.get_by_label("Run it").click();
    until(&mut h, "the warning", |h| {
        shown_part(h, "removes more than the default")
    });
    let pane = state(&h).cleanup.pane.clone().expect("the pane");
    assert!(pane.armed && pane.dry_run);
    h.event(egui::Event::Text("y".into()));
    until(&mut h, "the run", |h| {
        shown(h, "Done; the directory is rescanned.")
    });
    assert!(tmp.path().join("proj").join("build").join("a.o").exists());
    h.key_press(Key::Escape);
    until(&mut h, "the pane to close", |h| {
        state(h).cleanup.pane.is_none()
    });
    close(h);
}

#[cfg(unix)]
#[test]
fn a_read_only_browser_runs_only_the_dry_run() {
    let tmp = project();
    let journal = tempfile::tempdir().expect("temp dir");
    let mut h = opened(tmp.path(), true, journal.path(), 1);
    select_build(&mut h);
    h.event(egui::Event::Text("N".into()));
    until(&mut h, "the dry run", |h| {
        shown(h, "That was the dry run; read-only mode runs nothing more.")
    });
    assert!(!shown(&h, "Run it"));
    h.event(egui::Event::Text("y".into()));
    h.step();
    let s = state(&h);
    assert_eq!(s.status.as_deref(), Some(READ_ONLY));
    assert!(s.cleanup.pane.as_ref().is_some_and(|p| p.dry_run));
    close(h);
}

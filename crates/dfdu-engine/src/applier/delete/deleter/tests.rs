use dfdu_clean::Mode;
use dfdu_tree::{HostPath, NodeFlags, RuleIx};

use super::*;
use crate::applier::delete::tests::rig;
use crate::applier::test_util::Rig;
use crate::applier::{Command, NativeRun};

/// The notices `msg` adds.
fn noticed(r: &mut Rig, msg: DeleterMsg) -> Vec<String> {
    let before = r.m.status.notices.len();
    r.a.apply_delete(&mut r.m, msg);
    r.m.status.notices[before..].to_vec()
}

/// A native command `id` running, its output already the most kept.
fn native(r: &mut Rig, id: u64) {
    r.m.status.native = Some(NativeRun {
        id,
        rule: RuleIx(0),
        command: "true".into(),
        cwd: None,
        dry_run: false,
        lines: (0..NATIVE_LINES).map(|i| i.to_string()).collect(),
        done: None,
    });
}

/// The end of the running plan unmarks what it did not answer for, keeps its summary with
/// btrfs's notice, and the end of another plan changes nothing.
#[test]
fn a_plans_end_unmarks_what_is_left_and_keeps_its_summary() {
    let mut r = rig();
    let f = r.m.tree.node_ref(r.at("f"));
    r.command(Command::Delete {
        items: vec![f],
        mode: Mode::Permanent,
    });
    let plan = r.a.take_plans()[0].id;
    assert!(r.m.tree.flags(r.at("f")).has(NodeFlags::DELETING));
    r.a.apply_delete(
        &mut r.m,
        DeleterMsg::Done {
            plan: plan + 1,
            btrfs: false,
        },
    );
    assert!(r.m.status.last_delete.is_none(), "not the running plan");
    let new = noticed(&mut r, DeleterMsg::Done { plan, btrfs: true });
    assert!(new[0].starts_with("btrfs frees the space"), "{new:?}");
    assert!(!r.m.tree.flags(r.at("f")).has(NodeFlags::DELETING));
    let s = r.m.status.last_delete.as_ref().unwrap();
    assert_eq!((s.plan, s.deleted, s.btrfs), (plan, 0, true));
}

/// A staged item deleted in the background leaves the staged count, and says why when that
/// failed.
#[test]
fn a_commit_leaves_the_staged_count() {
    let mut r = rig();
    r.m.status.staged.add(100);
    r.m.status.staged.add(50);
    let new = noticed(
        &mut r,
        DeleterMsg::Committed {
            plan: 1,
            bytes: 100,
            result: Err("busy".into()),
        },
    );
    assert_eq!(new, ["delete staged: busy"]);
    let staged = &r.m.status.staged;
    assert_eq!((staged.items, staged.bytes), (1, 50));
}

/// What an undo put back where something else is now is named; what it could not put back
/// says why; and the end of the undo clears it.
#[test]
fn an_undo_says_what_it_put_back_elsewhere_or_could_not() {
    let mut r = rig();
    r.m.status.staged.add(10);
    r.m.status.undoing = Some(3);
    let restored = |original: &str, result| DeleterMsg::Restored {
        plan: 3,
        original: HostPath::from(original),
        staged: true,
        bytes: 10,
        result,
    };
    let as_is = noticed(&mut r, restored("/r/x", Ok(HostPath::from("/r/x"))));
    assert!(as_is.is_empty(), "{as_is:?}");
    assert_eq!(r.m.status.staged.items, 0);
    let elsewhere = restored("/r/y", Ok(HostPath::from("/r/y.restored-1")));
    let new = noticed(&mut r, elsewhere);
    assert_eq!(
        new,
        [format!(
            "undo: {} exists again; restored as {}",
            HostPath::from("/r/y").display(),
            HostPath::from("/r/y.restored-1").display()
        )]
    );
    let failed = noticed(&mut r, restored("/r/z", Err("gone".into())));
    assert_eq!(
        failed,
        [format!("undo {}: gone", HostPath::from("/r/z").display())]
    );
    r.a.apply_delete(&mut r.m, DeleterMsg::UndoDone { plan: 2 });
    assert_eq!(r.m.status.undoing, Some(3), "another undo's end");
    r.a.apply_delete(&mut r.m, DeleterMsg::UndoDone { plan: 3 });
    assert_eq!(r.m.status.undoing, None);
}

/// A native command keeps its last lines of output and its result; another command's
/// messages are dropped.
#[test]
fn a_native_command_keeps_its_last_lines_and_result() {
    let mut r = rig();
    native(&mut r, 7);
    for id in [7, 8] {
        let line = format!("from {id}");
        r.a.apply_delete(&mut r.m, DeleterMsg::NativeOutput { id, line });
    }
    let run = r.m.status.native.as_ref().unwrap();
    assert_eq!(run.lines.len(), NATIVE_LINES);
    assert_eq!(run.lines.front().map(String::as_str), Some("1"));
    assert_eq!(run.lines.back().map(String::as_str), Some("from 7"));
    let done = |id| DeleterMsg::NativeDone {
        id,
        result: Err("exit 1".into()),
    };
    r.a.apply_delete(&mut r.m, done(8));
    assert_eq!(r.m.status.native.as_ref().unwrap().done, None);
    r.a.apply_delete(&mut r.m, done(7));
    let run = r.m.status.native.as_ref().unwrap();
    assert_eq!(run.done, Some(Err("exit 1".into())));
}

/// Deletes an earlier session left unfinished are told of once, counted, with the journal's
/// notes.
#[test]
fn unfinished_deletes_are_noticed() {
    let mut r = rig();
    let new = noticed(
        &mut r,
        DeleterMsg::Unfinished {
            items: vec!["/x".into(), "/y".into(), "/z".into()],
            notes: vec!["torn record".into()],
        },
    );
    assert_eq!(new.len(), 2, "{new:?}");
    assert!(new[0].contains("left 3 deletes unfinished"), "{}", new[0]);
    assert!(new[0].ends_with(": /x; and 2 more"), "{}", new[0]);
    assert_eq!(new[1], "deletion journal: torn record");
}

/// One unfinished delete is named alone; no item and no note says nothing.
#[test]
fn one_unfinished_delete_is_named_alone() {
    let mut status = Status::default();
    notice_unfinished(&mut status, &["/x".into()], vec![]);
    assert_eq!(status.notices.len(), 1);
    let n = &status.notices[0];
    assert!(n.contains("left 1 delete unfinished"), "{n}");
    assert!(n.ends_with(": /x"), "{n}");
    notice_unfinished(&mut status, &[], vec![]);
    assert_eq!(status.notices.len(), 1);
}

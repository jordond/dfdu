use dfdu_rules::RuleIx;
use dfdu_tree::{ErrKind, Kind};

use super::super::ToAgent;
use super::super::tests::plan;
use super::*;
use crate::classifier::CheckReq;

/// The built-in rules for Linux without any variables, which lack the native rules of macOS.
fn few_rules() -> RuleSet {
    dfdu_rules::load_from(
        &dfdu_rules::Env::with(dfdu_rules::Platform::Linux, &[]),
        &[],
    )
    .0
}

/// `m` read back after `enc_del_msg`, every byte consumed.
fn del_msg_back(m: &DeleterMsg) -> R<DeleterMsg> {
    let mut e = Enc::default();
    enc_del_msg(&mut e, m);
    let mut d = Dec::new(&e.0);
    let back = dec_del_msg(&mut d)?;
    d.end()?;
    Ok(back)
}

/// The result of item message `m`.
fn result(m: DeleterMsg) -> DeleteResult {
    match m {
        DeleterMsg::Item { result, .. } => result,
        other => panic!("not an item: {other:?}"),
    }
}

#[test]
fn every_item_result_reads_back() {
    let refusals = [
        Refusal::Protected,
        Refusal::OutsideScanRoot,
        Refusal::ScanRoot,
        Refusal::MountPoint,
        Refusal::ContainsMount("/m/\u{e9}".into()),
        Refusal::TrashCrossesFilesystems,
        Refusal::TrashOtherSubvolume("/sv".into()),
        Refusal::NoJournal,
    ];
    let results = refusals.into_iter().map(DeleteResult::Refused).chain([
        DeleteResult::Deleted,
        DeleteResult::Stale("changed".into()),
        DeleteResult::Aborted,
        DeleteResult::Failed {
            partial: false,
            err: ErrKind::PermissionDenied,
            msg: "denied".into(),
        },
    ]);
    for r in results {
        let m = DeleterMsg::Item {
            plan: 9,
            node: NodeRef::from_bits(3),
            bytes: 1 << 33,
            result: r.clone(),
        };
        assert_eq!(result(del_msg_back(&m).unwrap()), r);
    }
}

/// Tag 5 was a read-only refusal no deleter sent; it, and tags past the last, are refused.
#[test]
fn retired_and_unknown_refusals_are_refused() {
    let m = DeleterMsg::Item {
        plan: 1,
        node: NodeRef::from_bits(1),
        bytes: 0,
        result: DeleteResult::Refused(Refusal::NoJournal),
    };
    let mut e = Enc::default();
    enc_del_msg(&mut e, &m);
    for tag in [5, 9] {
        *e.0.last_mut().unwrap() = tag;
        assert!(dec_del_msg(&mut Dec::new(&e.0)).is_err(), "refusal {tag}");
    }
}

#[test]
fn a_plan_listing_a_directory_twice_is_refused() {
    let mut p = plan();
    let mut e = Enc::default();
    enc_del_req(&mut e, &DeleterReq::Plan(p.clone()));
    let mut d = Dec::new(&e.0);
    let DeleterReq::Plan(back) = dec_del_req(&mut d, None).unwrap() else {
        panic!("not a plan");
    };
    d.end().unwrap();
    assert_eq!((back.id, back.mode, back.items.len()), (p.id, p.mode, 1));
    assert_eq!(back.items[0].inside, p.items[0].inside);
    // Listings are keyed by path, so a twice-listed one is made on the wire: two listings,
    // the name of the first (its length, `a`, its inode) patched to the second's.
    let mut inside = Listings::default();
    for rel in ["a", "b"] {
        inside.insert(
            rel.into(),
            Listing {
                ino: 1,
                entries: vec![(7, 8, Kind::File)],
            },
        );
    }
    p.items[0].inside = Arc::new(inside);
    let mut e = Enc::default();
    enc_del_req(&mut e, &DeleterReq::Plan(p));
    let at = e.0.windows(3).position(|w| w == [1, b'a', 1]).unwrap();
    e.0[at + 1] = b'b';
    assert!(dec_del_req(&mut Dec::new(&e.0), None).is_err());
}

/// Native commands and sizes go as their rule; the agent builds them from its rules.
/// (Docker's rule, with variants and a size command, is for macOS and Windows.)
#[test]
fn native_commands_go_as_their_rule() {
    let set = dfdu_rules::builtin(&dfdu_rules::Env::with(
        dfdu_rules::Platform::MacOs,
        &[("HOME", "/Users/u")],
    ))
    .0;
    let docker = set
        .rules()
        .iter()
        .position(|r| {
            r.action
                .native()
                .is_some_and(|n| !n.variants.is_empty() && !n.size_command.is_empty())
        })
        .map(|i| RuleIx(u16::try_from(i).unwrap()))
        .unwrap();
    let path = std::path::Path::new("/srv/\u{e9}t\u{e9}");
    for (variant, dry_run) in [(None, false), (None, true), (Some(0), false)] {
        let want = crate::applier::native_req(&set, docker, variant, path, dry_run, 5)
            .expect("the rule has the command");
        let d = ToAgent::Delete(DeleterReq::Native(want.clone()));
        let ToAgent::Delete(DeleterReq::Native(got)) =
            ToAgent::decode(&d.encode(), Some(&set)).unwrap()
        else {
            panic!("not a native command");
        };
        assert_eq!(got, want);
        assert!(ToAgent::decode(&d.encode(), None).is_err());
        assert!(ToAgent::decode(&d.encode(), Some(&few_rules())).is_err());
    }
    // A variant the rule does not have, a relative path, or no rule at all are refused.
    let mut bad = crate::applier::native_req(&set, docker, None, path, false, 5).unwrap();
    bad.rule.as_mut().unwrap().variant = Some(99);
    let bytes = ToAgent::Delete(DeleterReq::Native(bad.clone())).encode();
    assert!(ToAgent::decode(&bytes, Some(&set)).is_err());
    bad.rule.as_mut().unwrap().variant = None;
    bad.rule.as_mut().unwrap().path = "srv".into();
    let bytes = ToAgent::Delete(DeleterReq::Native(bad.clone())).encode();
    assert!(ToAgent::decode(&bytes, Some(&set)).is_err());
    bad.rule = None;
    let bytes = ToAgent::Delete(DeleterReq::Native(bad)).encode();
    assert!(ToAgent::decode(&bytes, Some(&set)).is_err());
    let size = ToAgent::Check(vec![CheckReq::NativeSize {
        rule: docker,
        check: vec![],
        command: vec!["anything".into()],
    }]);
    let ToAgent::Check(got) = ToAgent::decode(&size.encode(), Some(&set)).unwrap() else {
        panic!("not a check");
    };
    let n = set.rule(docker).action.native().unwrap();
    assert!(
        matches!(&got[..], [CheckReq::NativeSize { rule, check, command }]
        if *rule == docker && *check == n.check && *command == n.size_command)
    );
    let plain = set
        .rules()
        .iter()
        .position(|r| r.action.native().is_none())
        .unwrap();
    let plain = RuleIx(u16::try_from(plain).unwrap());
    let none = ToAgent::Check(vec![CheckReq::NativeSize {
        rule: plain,
        check: vec![],
        command: vec![],
    }]);
    assert!(ToAgent::decode(&none.encode(), Some(&set)).is_err());
}

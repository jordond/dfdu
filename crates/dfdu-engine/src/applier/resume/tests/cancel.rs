//! Cancelled resumes: what they did not list again or check stays as loaded, the scan
//! incomplete, and deletes and native commands refuse it until a rescan lists it again.

use super::*;

/// Deletes of `items` sent to `ap`, refused as not refreshed by a cancelled resume.
fn refused_as_unverified<B: Backend>(
    ap: &mut Applier<B>,
    m: &mut Model,
    items: Vec<NodeRef>,
) -> usize {
    let before = m.status.notices.len();
    ap.apply_command(
        m,
        Command::Delete {
            items,
            mode: dfdu_clean::Mode::Permanent,
        },
    );
    m.status
        .notices
        .iter()
        .skip(before)
        .filter(|n| n.contains("its resume was cancelled"))
        .count()
}

/// While a resume walks the loaded tree, what would touch it is refused; a cancel while it
/// walks starts none of its rescans, which leaves the tree as loaded: the scan is incomplete,
/// and deletes refuse what the resume did not refresh, which a rescan may list again.
#[test]
#[expect(
    clippy::many_single_char_names,
    reason = "nodes named as in the fixture's diagram"
)]
fn a_resume_walking_refuses_and_cancels_what_follows() {
    use dfdu_clean::Mode;

    let (t, [r, a, _, c]) = tree();
    let (a, c) = (t.node_ref(a), t.node_ref(c));
    let (mut ap, mut m) = resumed(t, r, &["a/b", "c"], 0, None);
    ap.check(&m).unwrap();
    let command = |ap: &mut Applier<PortableBackend>, m: &mut Model, cmd| {
        ap.apply_command(m, cmd);
        ap.check(m).unwrap();
    };
    assert!(ap.resuming());
    command(&mut ap, &mut m, Command::Rescan { node: c });
    command(
        &mut ap,
        &mut m,
        Command::Delete {
            items: vec![a],
            mode: Mode::Permanent,
        },
    );
    let refused = m
        .status
        .notices
        .iter()
        .filter(|n| n.contains("still being taken over"))
        .count();
    assert_eq!(refused, 2, "{:?}", m.status.notices);
    command(&mut ap, &mut m, Command::CancelAll);
    while ap.has_work() {
        ap.work_step(&mut m);
        ap.check(&m).unwrap();
    }
    assert!(!ap.jobs.values().any(|j| j.kind == JobKind::Rescan));
    assert!(m.status.incomplete);
    assert_eq!(refused_as_unverified(&mut ap, &mut m, vec![a, c]), 2);
    assert!(!unrefreshed(&m.tree, m.tree.resolve_visible(c).unwrap()));
    command(&mut ap, &mut m, Command::Rescan { node: c });
    assert!(ap.jobs.values().any(|j| j.kind == JobKind::Rescan));
}

/// Once the walk has taken the tree over, the resume waits for the check of the directories
/// it keeps, refusing what would touch the tree as while it walks; a cancel then ends the
/// wait, starting none of the rescans, and an answer that comes late changes nothing. What
/// the check did not vouch for stays refused to deletes.
#[test]
#[expect(
    clippy::many_single_char_names,
    reason = "nodes named as in the fixture's diagram"
)]
fn a_resume_checking_refuses_and_cancels_what_follows() {
    use dfdu_clean::Mode;
    use dfdu_scan::ScanMsg;

    let (t, [r, a, _, c]) = tree();
    let (a, c) = (t.node_ref(a), t.node_ref(c));
    let (mut ap, mut m) = resumed(t, r, &[], 0, None);
    while ap.has_work() {
        ap.work_step(&mut m);
    }
    // No worker runs here: the check's one batch waits.
    assert_eq!(ap.scan.outstanding(), (1, 0));
    assert!(ap.resuming() && m.status.settling && m.status.busy());
    ap.apply_command(&mut m, Command::Rescan { node: c });
    ap.apply_command(
        &mut m,
        Command::Delete {
            items: vec![a],
            mode: Mode::Permanent,
        },
    );
    let refused = m
        .status
        .notices
        .iter()
        .filter(|n| n.contains("still being taken over"))
        .count();
    assert_eq!(refused, 2, "{:?}", m.status.notices);
    ap.apply_command(&mut m, Command::CancelAll);
    while ap.has_work() {
        ap.work_step(&mut m);
        ap.check(&m).unwrap();
    }
    assert!(!ap.resuming());
    assert!(!ap.jobs.values().any(|j| j.kind == JobKind::Rescan));
    assert!(m.status.incomplete);
    assert_eq!(refused_as_unverified(&mut ap, &mut m, vec![a, c]), 2);
    let job = ap.jobs.keys().next().copied().unwrap();
    ap.apply(
        &mut m,
        ScanMsg::Verified {
            job: JobId(job),
            batch: 0,
            changed: vec![0, 1],
            gone: vec![],
            complete: true,
        },
        1,
    );
    assert!(!ap.has_work() && !ap.resuming());
    ap.check(&m).unwrap();
}

/// A resume's rescan cancelled once the rescans started leaves what it was to list again as
/// loaded: the scan is incomplete, and deletes and native commands refuse that directory,
/// what is below it and what holds it, while the rest of what the resume checked is deleted
/// from. Once the directory is rescanned, it is deleted from too.
#[test]
#[expect(
    clippy::many_single_char_names,
    reason = "nodes named as in the fixture's diagram"
)]
fn a_cancelled_rescan_of_a_resume_leaves_its_directory_refused() {
    use dfdu_scan::ScanMsg;

    let (t, [r, a, b, c]) = tree();
    let (root, a, b, c) = (t.node_ref(r), t.node_ref(a), t.node_ref(b), t.node_ref(c));
    let stat = crate::applier::meta_of(&t, r);
    let (mut ap, mut m) = resumed(t, r, &["a"], 0, Some(stat));
    while ap.has_work() {
        ap.work_step(&mut m);
    }
    let job = ap.jobs.keys().next().copied().unwrap();
    ap.apply(
        &mut m,
        ScanMsg::Verified {
            job: JobId(job),
            batch: 0,
            changed: vec![],
            gone: vec![],
            complete: true,
        },
        1,
    );
    while ap.has_work() {
        ap.work_step(&mut m);
    }
    assert!(!ap.resuming() && !m.status.incomplete);
    let rescans: Vec<u32> = ap
        .jobs
        .iter()
        .filter(|(_, j)| j.kind == JobKind::Rescan)
        .map(|(&id, _)| id)
        .collect();
    assert_eq!(rescans.len(), 1, "of `a`, the root being as loaded");
    ap.apply_command(
        &mut m,
        Command::Cancel {
            job: JobId(rescans[0]),
        },
    );
    while ap.has_work() {
        ap.work_step(&mut m);
        ap.check(&m).unwrap();
    }
    assert!(m.status.incomplete);
    assert_eq!(refused_as_unverified(&mut ap, &mut m, vec![a, b, root]), 3);
    assert_eq!(refused_as_unverified(&mut ap, &mut m, vec![c]), 0);
    ap.apply_command(
        &mut m,
        Command::RunNative {
            id: 1,
            node: b,
            variant: None,
            dry_run: false,
        },
    );
    assert_eq!(
        m.status.notices.last().map(String::as_str),
        Some(UNVERIFIED_NATIVE)
    );
    // A rescan of it is taken, which lists it again.
    ap.apply_command(&mut m, Command::Rescan { node: a });
    assert_eq!(ap.jobs.values().filter(|j| j.swap_pending()).count(), 1);
}

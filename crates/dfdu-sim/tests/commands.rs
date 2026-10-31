//! Scripted scenarios for commands against the scan: rescans and deletes against running scans,
//! plans and each other, unfinished deletes, hard links across a delete or rescan, marks across
//! a rescan swap, and cancels. Each forces its sequence by hand, checks what it must lead to,
//! then finishes the run in random order with every check of the random simulation, for several
//! seeds.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use dfdu_scan::{DirId, ListingStatus, ScanMsg};
use dfdu_sim::{Sim, Spec, SpecBuilder, quiet_injected_panics};
use dfdu_tree::DirState;

use common::{
    cfg, find, finish_jobs, for_seeds, idle, ids, node, rescan, runs, scan_root, state, step_until,
};

fn rescans_running(sim: &Sim) -> usize {
    sim.model
        .status
        .jobs
        .iter()
        .filter(|j| {
            j.kind == dfdu_engine::JobKind::Rescan
                && matches!(
                    j.phase,
                    dfdu_engine::JobPhase::Running | dfdu_engine::JobPhase::Finishing
                )
        })
        .count()
}

fn last_notice(sim: &Sim) -> &str {
    sim.model.status.notices.last().map_or("", |s| s.as_str())
}

fn delete(sim: &mut Sim, paths: &[&str]) {
    let items = paths.iter().map(|p| node(sim, p)).collect();
    sim.command(dfdu_engine::Command::Delete {
        items,
        mode: dfdu_engine::Mode::Permanent,
    })
    .unwrap();
}

/// `/a/b/c` with two files at each level, and `/d` with two files.
fn nested() -> Spec {
    let mut b = SpecBuilder::new();
    let mut cur = dfdu_sim::ROOT;
    for name in ["a", "b", "c"] {
        cur = b.dir(cur, name);
        b.files(cur, &format!("{name}f"), 2);
    }
    let d = b.dir(dfdu_sim::ROOT, "d");
    b.files(d, "df", 2);
    b.build()
}

/// Rescans against delete plans: a rescan of the item, an ancestor or a descendant of the running
/// or a queued plan is rejected, including a rescan requested while a delete in its subtree runs;
/// once the plans are done, it is accepted.
#[test]
fn rescan_while_delete_overlaps() {
    for_seeds(nested, cfg(), |sim| {
        finish_jobs(sim);
        delete(sim, &["a/b"]);
        assert_eq!(sim.running_plan().map(|p| p.len()), Some(1));
        for target in ["a", "a/b", "a/b/c"] {
            rescan(sim, target);
            assert!(
                last_notice(sim).contains("delete in this directory"),
                "{target}: {}",
                last_notice(sim)
            );
            assert_eq!(rescans_running(sim), 0);
        }
        // A second plan is queued behind the first; a rescan of its item is rejected too.
        delete(sim, &["d"]);
        assert!(sim.running_plan().unwrap()[0].ends_with("b"));
        rescan(sim, "d");
        assert!(
            last_notice(sim).contains("delete in this directory"),
            "{}",
            last_notice(sim)
        );
        assert_eq!(rescans_running(sim), 0);
        // The first plan deletes `b`; the queued plan starts.
        assert!(sim.delete_next(dfdu_engine::DeleteResult::Deleted).unwrap());
        assert!(sim.delete_next(dfdu_engine::DeleteResult::Deleted).unwrap());
        sim.applier_work().unwrap();
        assert!(sim.running_plan().unwrap()[0].ends_with("d"));
        rescan(sim, "a");
        assert_eq!(rescans_running(sim), 1, "a is no longer in a plan");
        assert!(
            sim.delete_next(dfdu_engine::DeleteResult::Stale("changed".into()))
                .unwrap()
        );
        assert!(sim.delete_next(dfdu_engine::DeleteResult::Deleted).unwrap());
    });
}

/// Rescans and deletes against a running rescan: a delete of the target, an ancestor or a
/// descendant of a running rescan is refused; a rescan of the target or a descendant is rejected; a
/// rescan of an ancestor cancels the running one; a rescan of a detached directory is rejected.
#[test]
fn rescans_and_deletes_against_rescans() {
    for_seeds(nested, cfg(), |sim| {
        finish_jobs(sim);
        rescan(sim, "a/b");
        assert_eq!(rescans_running(sim), 1);
        for target in ["a", "a/b", "a/b/c"] {
            delete(sim, &[target]);
            assert!(
                last_notice(sim).contains("being rescanned"),
                "{target}: {}",
                last_notice(sim)
            );
            assert!(sim.running_plan().is_none());
        }
        for target in ["a/b", "a/b/c"] {
            rescan(sim, target);
            assert!(
                last_notice(sim).contains("already rescanning"),
                "{target}: {}",
                last_notice(sim)
            );
        }
        let old_a = node(sim, "a");
        rescan(sim, "a");
        assert_eq!(rescans_running(sim), 1, "the rescan of a/b was cancelled");
        finish_jobs(sim);
        assert!(sim.model.tree.resolve(old_a).is_none(), "swapped out");
        sim.command(dfdu_engine::Command::Rescan { node: old_a })
            .unwrap();
        assert!(last_notice(sim).contains("gone"), "{}", last_notice(sim));
        assert_eq!(rescans_running(sim), 0);
    });
}

/// A directory the initial scan is still reading: a rescan cuts it, and it
/// reports `Superseded` to its parent; a delete cuts it, and it reports `Deleting`.
#[test]
fn rescan_and_delete_inside_running_scan() {
    for_seeds(nested, cfg(), |sim| {
        scan_root(sim);
        assert_eq!(state(sim, "a"), DirState::Pending);
        rescan(sim, "a");
        assert_eq!(state(sim, "a"), DirState::Superseded);
        assert_eq!(rescans_running(sim), 1);
        delete(sim, &["d"]);
        assert_eq!(state(sim, "d"), DirState::Deleting);
        assert!(sim.running_plan().is_some());
    });
}

/// A delete of a directory still being scanned, then late messages for its descendants:
/// those that arrive while it is cut and those that arrive after it was detached are
/// discarded, and none counts as corrupt or makes anything lost. Worker 0 holds `keep` open,
/// so the job is still running when the late messages arrive.
#[test]
fn delete_while_scanning_then_late_messages() {
    for_seeds(
        || {
            let mut b = SpecBuilder::new();
            let keep = b.dir(dfdu_sim::ROOT, "keep");
            b.files(keep, "k", 2);
            let d = b.dir(dfdu_sim::ROOT, "d");
            for name in ["e", "f"] {
                let x = b.dir(d, name);
                b.files(x, "x", 2);
            }
            b.file(d, "g", 5);
            b.build()
        },
        cfg(),
        |sim| {
            scan_root(sim);
            // Worker 1 lists `d`, then `e` from its private stack, sending nothing.
            let d = ids(sim, "d");
            step_until(sim, 1, |s| runs(s, 1, d, 0));
            step_until(sim, 0, |s| runs(s, 0, ids(s, "keep"), 0));
            step_until(sim, 1, |s| idle(s, 1));
            step_until(sim, 1, |s| runs(s, 1, (d.0, DirId(d.1.0 + 1)), 0));
            step_until(sim, 1, |s| idle(s, 1));
            assert!(sim.queued().is_empty());
            delete(sim, &["d"]);
            assert_eq!(state(sim, "d"), DirState::Deleting);
            // `d`'s chunk naming `e` and `f`, its Close and `e`'s messages: under the cut.
            sim.flush_worker(1);
            sim.apply_all().unwrap();
            let cut = sim.model.status.stats.discarded_cut;
            assert!(cut >= 4, "{cut}");
            // After the detach, `f`'s task (published by that flush) finds its stop bit; its
            // Close is discarded too.
            assert!(sim.delete_next(dfdu_engine::DeleteResult::Deleted).unwrap());
            sim.applier_work().unwrap();
            step_until(sim, 2, |s| {
                s.workers[2].slot.outbox.lock().messages().iter().any(|m| {
                    matches!(
                        m,
                        ScanMsg::Close {
                            status: ListingStatus::Cancelled,
                            ..
                        }
                    )
                })
            });
            sim.flush_worker(2);
            sim.apply_all().unwrap();
            let st = &sim.model.status.stats;
            assert!(st.discarded_cut > cut && st.discarded_job == 0, "{st:?}");
            assert_eq!((st.corrupt, st.lost, st.duplicate), (0, 0, 0));
            assert!(sim.delete_next(dfdu_engine::DeleteResult::Deleted).unwrap());
        },
    );
}

/// The results the unfinished-delete scenarios run through, and how each counts in the plan's summary
/// `(stale, refused, failed, aborted)`.
fn unfinished_results() -> Vec<(dfdu_engine::DeleteResult, (usize, usize, usize, usize))> {
    use dfdu_clean::Refusal;
    use dfdu_engine::DeleteResult;
    let failed = |partial| DeleteResult::Failed {
        partial,
        err: dfdu_tree::ErrKind::Other,
        msg: "eio".into(),
    };
    vec![
        (DeleteResult::Refused(Refusal::Protected), (0, 1, 0, 0)),
        (DeleteResult::Aborted, (0, 0, 0, 1)),
        (DeleteResult::Stale("changed".into()), (1, 0, 0, 0)),
        (failed(false), (0, 0, 1, 0)),
        (failed(true), (0, 0, 1, 0)),
    ]
}

/// Answers the running plan's items with `result`, removing `gone` from the filesystem first
/// (what a part-way delete took), then ends the plan. Checks that each item is still in the
/// tree and no longer marked as being deleted, that a rescan started exactly for `rescanned`,
/// marked cut until it completes, and the plan's summary.
fn answer_plan(
    sim: &mut Sim,
    items: &[&str],
    result: &dfdu_engine::DeleteResult,
    counts: (usize, usize, usize, usize),
    gone: Option<&str>,
    rescanned: bool,
) {
    use dfdu_scan::Backend;
    if let Some(g) = gone {
        let path = std::path::Path::new(dfdu_sim::ROOT_NAME).join(g);
        let n = sim.shared.backend.loc_from_path(&path);
        sim.spec.delete_subtree(n);
    }
    assert_eq!(rescans_running(sim), 0);
    for _ in items {
        assert!(sim.delete_next(result.clone()).unwrap());
    }
    assert!(
        sim.delete_next(dfdu_engine::DeleteResult::Deleted).unwrap(),
        "Done"
    );
    assert!(sim.running_plan().is_none());
    for p in items {
        let ix = find(sim, p);
        assert!(
            !sim.model.tree.flags(ix).has(dfdu_tree::NodeFlags::DELETING),
            "{p}"
        );
    }
    let dir = find(sim, items[0]);
    assert_eq!(rescans_running(sim), usize::from(rescanned), "{result:?}");
    if rescanned {
        assert!(sim.model.tree.dir(dir).unwrap().cut, "{result:?}");
    }
    let s = sim.model.status.last_delete.clone().expect("summary");
    assert_eq!(
        (s.deleted, (s.stale, s.refused, s.failed, s.aborted)),
        (
            0,
            (
                counts.0 * items.len(),
                counts.1 * items.len(),
                counts.2 * items.len(),
                counts.3 * items.len()
            )
        ),
        "{result:?}"
    );
    assert!(
        last_notice(sim).starts_with(&format!(
            "delete {}: ",
            items.last().unwrap().rsplit('/').next().unwrap()
        )),
        "{}",
        last_notice(sim)
    );
}

/// Deletes that are refused, aborted, stale and failed (whole or part-way), on a
/// directory and a file the scan had finished. Each stays in the tree with its totals; only a
/// directory a delete emptied part-way is rescanned, and the random tail checks it against the
/// filesystem.
#[test]
fn unfinished_deletes_on_uncut_nodes() {
    for_seeds(nested, cfg(), |sim| {
        finish_jobs(sim);
        for (result, counts) in unfinished_results() {
            let partial = matches!(
                result,
                dfdu_engine::DeleteResult::Failed { partial: true, .. }
            );
            let before = sim.model.tree.total(find(sim, "a")).alloc;
            delete(sim, &["a/b", "d/df0"]);
            assert_eq!(
                state(sim, "a/b"),
                DirState::Complete,
                "a finished directory is not cut"
            );
            answer_plan(
                sim,
                &["a/b", "d/df0"],
                &result,
                counts,
                partial.then_some("a/b/c"),
                partial,
            );
            if partial {
                finish_jobs(sim);
                assert!(!sim.model.tree.dir(find(sim, "a/b")).unwrap().cut);
                assert!(sim.model.tree.total(find(sim, "a")).alloc < before);
            } else {
                assert_eq!(sim.model.tree.total(find(sim, "a")).alloc, before);
            }
        }
    });
}

/// Unfinished deletes on cut nodes: a directory the scan is still reading is cut by the delete, so
/// whatever the result, its totals are unknown and it is rescanned once the plan no longer
/// holds it; until then it stays marked cut.
#[test]
fn unfinished_deletes_on_cut_nodes() {
    for (result, counts) in unfinished_results() {
        let partial = matches!(
            result,
            dfdu_engine::DeleteResult::Failed { partial: true, .. }
        );
        for_seeds(nested, cfg(), |sim| {
            scan_root(sim);
            assert_eq!(state(sim, "a"), DirState::Pending);
            delete(sim, &["a"]);
            assert_eq!(state(sim, "a"), DirState::Deleting);
            answer_plan(sim, &["a"], &result, counts, partial.then_some("a/b"), true);
            finish_jobs(sim);
            let a = find(sim, "a");
            assert!(!sim.model.tree.dir(a).unwrap().cut, "{result:?}");
            assert_eq!(state(sim, "a"), DirState::Complete, "{result:?}");
        });
    }
}

/// The entries of `paths` that hold their inode's charge.
fn charged(sim: &Sim, paths: &[&str]) -> Vec<String> {
    paths
        .iter()
        .filter(|p| {
            sim.model
                .tree
                .flags(find(sim, p))
                .has(dfdu_tree::NodeFlags::CHARGED)
        })
        .map(std::string::ToString::to_string)
        .collect()
}

/// A hard-linked inode with links inside and outside a deleted subtree, then inside and
/// outside a rescanned subtree. The charge moves to the lowest remaining path after the
/// delete, and stays single after the rescan's scope is merged.
#[test]
#[expect(
    clippy::many_single_char_names,
    reason = "short names for the spec's directories and files, as in the other scenarios"
)]
fn hard_links_across_delete_and_rescan() {
    for_seeds(
        || {
            let mut b = SpecBuilder::new();
            let x = b.dir(dfdu_sim::ROOT, "x");
            let f = b.file(x, "f", 50_000);
            let y = b.dir(dfdu_sim::ROOT, "y");
            b.link(y, "h", f);
            b.files(y, "o", 2);
            let z = b.dir(dfdu_sim::ROOT, "z");
            b.link(z, "k", f);
            b.build()
        },
        cfg(),
        |sim| {
            finish_jobs(sim);
            assert_eq!(charged(sim, &["x/f", "y/h", "z/k"]), ["x/f"]);
            let before = sim.model.tree.total(find(sim, "")).alloc;
            delete(sim, &["x"]);
            assert!(sim.delete_next(dfdu_engine::DeleteResult::Deleted).unwrap());
            assert!(sim.delete_next(dfdu_engine::DeleteResult::Deleted).unwrap());
            sim.applier_work().unwrap();
            assert_eq!(charged(sim, &["y/h", "z/k"]), ["y/h"]);
            // Only the directory `x` itself is gone: the data is still referenced.
            assert_eq!(sim.model.tree.total(find(sim, "")).alloc, before - 4096);
            rescan(sim, "y");
            finish_jobs(sim);
            assert_eq!(charged(sim, &["y/h", "z/k"]), ["y/h"]);
            rescan(sim, "z");
            finish_jobs(sim);
            assert_eq!(charged(sim, &["y/h", "z/k"]), ["y/h"]);
        },
    );
}

/// A Nix-like tree: a store of 40 files and 50 profiles linking each of them (2,000 links).
/// Rescanning the profiles swaps in a scope of 2,000 links. The merge runs in steps, with the
/// invariants holding after each, and every inode ends with one charge.
#[test]
fn dense_links_swapped_in() {
    let spec = || {
        let mut b = SpecBuilder::new();
        let store = b.dir(dfdu_sim::ROOT, "store");
        let files = b.files(store, "s", 40);
        let profiles = b.dir(dfdu_sim::ROOT, "profiles");
        for p in 0..50 {
            let d = b.dir(profiles, &format!("p{p}"));
            for (i, &f) in files.iter().enumerate() {
                b.link(d, &format!("l{i}"), f);
            }
        }
        b.build()
    };
    let mut cfg = cfg();
    cfg.limits.block_entries = 64;
    quiet_injected_panics();
    for seed in 0..3 {
        let mut sim = Sim::with_spec(seed, &cfg, spec());
        sim.record = true;
        finish_jobs(&mut sim);
        rescan(&mut sim, "profiles");
        // Until the job finishes, then count the steps of the merge.
        while sim.model.status.running_jobs() > 0 {
            for w in 0..sim.workers.len() {
                sim.step_worker(w);
            }
            sim.apply_all().unwrap();
            sim.watchdog();
            if sim.applier.has_work() {
                break;
            }
        }
        let mut steps = 0;
        while sim.applier.has_work() {
            sim.applier.work_step(&mut sim.model);
            sim.applier.refresh_status(&mut sim.model);
            sim.applier.check(&sim.model).unwrap();
            steps += 1;
            if steps % 64 == 0 {
                for w in 0..sim.workers.len() {
                    sim.step_worker(w);
                }
                sim.apply_all().unwrap();
            }
        }
        finish_jobs(&mut sim);
        assert!(steps > 10, "the swap ran in {steps} steps");
        let t = &sim.model.tree;
        let root = find(&sim, "");
        let store_alloc: u64 = (0..40u64).map(|i| (1000 + i).div_ceil(4096) * 4096).sum();
        let dirs = (1 + 1 + 1 + 50) * 4096;
        assert_eq!(t.total(root).alloc, store_alloc + dirs);
        if let Err(e) = sim.run_to_end() {
            panic!("{e}");
        }
    }
}

/// A mark on a file that is replaced by a different file with the same name before a rescan
/// swap. After the swap the mark's reference no longer resolves, and found again by path the
/// new file's identity differs, so the mark is reported gone instead of moving to it. A mark
/// on a file that was not replaced follows it.
#[test]
fn mark_on_replaced_file_is_gone_after_swap() {
    let spec = || {
        let mut b = SpecBuilder::new();
        let w = b.dir(dfdu_sim::ROOT, "w");
        b.file(w, "f", 100);
        let new = b.file(w, "f", 200);
        b.absent(new);
        b.file(w, "g", 300);
        b.build()
    };
    for_seeds(spec, cfg(), |sim| {
        finish_jobs(sim);
        let t = &sim.model.tree;
        let (f, g) = (
            dfdu_tree::Target::of(t, find(sim, "w/f")),
            dfdu_tree::Target::of(t, find(sim, "w/g")),
        );
        let w = sim.spec.nodes[0].children[0];
        let kids = sim.spec.nodes[w as usize].children.clone();
        sim.spec.delete_subtree(kids[0]);
        sim.spec.restore(kids[1]);
        rescan(sim, "w");
        finish_jobs(sim);
        let t = &sim.model.tree;
        assert_eq!(
            t.raw_size(find(sim, "w/f")).0,
            200,
            "the new f is in the tree"
        );
        assert_eq!(f.resolve(t), None, "the mark on the old f is gone");
        assert_eq!(
            g.resolve(t),
            Some(find(sim, "w/g")),
            "the mark on g follows it"
        );
    });
}

/// Cancelling the initial scan while a rescan inside it runs: the rescan is independent; it
/// finishes and swaps in.
#[test]
fn cancel_initial_scan_while_rescan_runs() {
    for_seeds(nested, cfg(), |sim| {
        scan_root(sim);
        rescan(sim, "a");
        let initial = sim.model.status.jobs[0].id;
        sim.command(dfdu_engine::Command::Cancel { job: initial })
            .unwrap();
        sim.applier_work().unwrap();
        assert_eq!(rescans_running(sim), 1);
        finish_jobs(sim);
        assert_eq!(state(sim, "a"), DirState::Complete);
        assert_eq!(state(sim, "a/b/c"), DirState::Complete);
    });
}

/// Cancelling a rescan whose target was superseded from the initial scan: the shadow is
/// dropped and the target stays visible, cut and incomplete; the initial scan goes on.
#[test]
fn cancel_rescan_of_superseded_directory() {
    for_seeds(nested, cfg(), |sim| {
        scan_root(sim);
        rescan(sim, "a");
        assert_eq!(state(sim, "a"), DirState::Superseded);
        let job = sim.model.status.jobs[1].id;
        sim.command(dfdu_engine::Command::Cancel { job }).unwrap();
        sim.applier_work().unwrap();
        assert_eq!(rescans_running(sim), 0);
        let a = sim.model.tree.dir(find(sim, "a")).unwrap();
        assert!(a.cut && a.state == DirState::Superseded);
        assert_eq!(sim.model.tree.roots().count(), 1);
        assert_eq!(
            sim.model.status.jobs[0].phase,
            dfdu_engine::JobPhase::Running
        );
    });
}

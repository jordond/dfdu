use dfdu_tree::{Kind, Meta};

use super::*;
use crate::matches::tests::{DAY, item, rule_with, rules};

/// A model of the one directory `/r`, complete or not.
fn scanned(complete: bool) -> (Model, Ix) {
    let mut m = Model::default();
    let meta = Meta {
        kind: Kind::Dir,
        apparent: 0,
        alloc: 0,
        mtime: 0,
        dev: 1,
        ino: 1,
        nlink: 1,
        mount: 0,
    };
    let root = m.tree.new_root(b"/r", &meta, true);
    m.status.incomplete = !complete;
    (m, root)
}

/// `--safety` keeps the matches as safe as the level given or safer, in their order; without
/// it every match is listed.
#[test]
fn safety_keeps_the_matches_at_most_that_level() {
    let rules = rules();
    let rule = rule_with(&rules, "delete");
    let list = || {
        vec![
            item(rule, Safety::Review, 3),
            item(rule, Safety::Safe, 2),
            item(rule, Safety::Protected, 1),
        ]
    };
    let disks = |l: Vec<Item>| l.iter().map(|i| i.disk).collect::<Vec<_>>();
    assert_eq!(disks(at_most(list(), None)), [3, 2, 1]);
    assert_eq!(disks(at_most(list(), Some(Safety::Review))), [3, 2]);
    assert_eq!(disks(at_most(list(), Some(Safety::Safe))), [2]);
}

/// The threshold is exceeded only past `--fail-above`, and protected matches do not count
/// towards it.
#[test]
fn the_threshold_leaves_protected_matches_out() {
    let rules = rules();
    let rule = rule_with(&rules, "delete");
    let list = [
        item(rule, Safety::Safe, 60),
        item(rule, Safety::Protected, 1000),
    ];
    assert!(!exceeded(&list, None));
    assert!(!exceeded(&list, Some(60)));
    assert!(exceeded(&list, Some(59)));
    assert!(!exceeded(&[], Some(0)));
}

/// Exceeding the threshold wins over an incomplete scan.
#[test]
fn exit_codes_put_the_threshold_first() {
    assert_eq!(exit_code(true, false), ExitCode::from(EXIT_THRESHOLD));
    assert_eq!(exit_code(true, true), ExitCode::from(EXIT_THRESHOLD));
    assert_eq!(exit_code(false, false), ExitCode::from(EXIT_PARTIAL));
    assert_eq!(exit_code(false, true), ExitCode::SUCCESS);
}

/// The document lists every match with the summary, says whether the scan is complete and how
/// long it took, and has a threshold only with `--fail-above`.
#[test]
fn the_document_lists_the_matches_and_the_threshold_when_given() {
    let rules = rules();
    let rule = rule_with(&rules, "delete");
    let list = [
        item(rule, Safety::Safe, 100),
        item(rule, Safety::Review, 20),
    ];
    let (m, root) = scanned(true);
    let elapsed = Duration::from_millis(1500);
    let doc = document(&m, root, &rules, &list, 10 * DAY, elapsed, Some(100));
    assert_eq!(doc["generated_at"], 10 * DAY);
    assert_eq!(doc["complete"], true);
    assert_eq!(doc["elapsed_ms"], 1500);
    let items = doc["items"].as_array().expect("items");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["disk"], 100);
    assert_eq!(doc["summary"]["disk"], 120);
    assert_eq!(
        doc["threshold"],
        json!({ "fail_above": 100, "exceeded": true })
    );
    let (m, root) = scanned(false);
    let doc = document(&m, root, &rules, &[], 0, Duration::ZERO, None);
    assert_eq!(doc["complete"], false);
    assert_eq!(doc["items"], json!([]));
    assert!(!doc.contains_key("threshold"));
}

use dfdu_rules::{Env, Platform};
use dfdu_tree::{Class, ClassFlags, HostPath, NodeRef, RuleIx};

use super::*;

const DAY: i64 = 86_400;

/// The built-in rules of Linux, the last of them made to report its matches only (none of the
/// built-in rules does).
fn rules() -> RuleSet {
    let env = Env::with(Platform::Linux, &[("HOME", "/home/u")]);
    let mut rules = dfdu_rules::builtin(&env).0.rules().to_vec();
    rules.last_mut().expect("a rule").action = Action::Report;
    RuleSet::new(rules, &env)
}

/// The first rule whose action is `action` (by its name in rule files).
fn rule_with(rules: &RuleSet, action: &str) -> RuleIx {
    let r = rules.rules().iter().find(|r| r.action.as_str() == action);
    rules
        .index(&r.unwrap_or_else(|| panic!("a {action} rule")).id)
        .expect("its index")
}

/// A directory matched by `rule` at `safety`, of `disk` bytes on disk.
fn item(rule: RuleIx, safety: Safety, disk: u64) -> Item {
    Item {
        node: NodeRef::from_bits(1),
        path: HostPath::from("/home/u/x"),
        kind: Kind::Dir,
        class: Class {
            rule,
            safety,
            degraded: None,
            flags: dfdu_tree::ClassFlags::NONE,
        },
        disk,
        apparent: disk / 2,
        entries: 3,
        shared_out: 0,
        mtime: 0,
        scanning: false,
        recommended: false,
        matches_inside: 0,
        rest: vec![],
    }
}

/// The table of `list` as `dfdu report` prints it.
fn printed(rules: &RuleSet, list: &[Item], fail_above: Option<u64>, exceeded: bool) -> String {
    let mut out = Vec::new();
    table(&mut out, rules, list, 10 * DAY, fail_above, exceeded).unwrap();
    String::from_utf8(out).unwrap()
}

#[test]
fn ages_are_whole_days_and_never_negative() {
    assert_eq!(age_days(10 * DAY, 10 * DAY), 0);
    assert_eq!(age_days(10 * DAY, 7 * DAY + 1), 2);
    assert_eq!(age_days(DAY, 5 * DAY), 0, "a match from the future is new");
}

#[test]
fn long_names_are_cut_to_their_width_in_characters() {
    assert_eq!(truncate("Rust target", 28), "Rust target");
    assert_eq!(truncate("abcdef", 6), "abcdef");
    assert_eq!(truncate("abcdefg", 6), "abcde…");
    assert_eq!(truncate("ééééééé", 4), "ééé…");
}

/// A protected match is kept whatever its rule does; the others are cleaned up the way their
/// rule says.
#[test]
fn each_match_says_how_it_is_cleaned_up() {
    let rules = rules();
    let delete = rule_with(&rules, "delete");
    assert_eq!(how(&rules, &item(delete, Safety::Safe, 1)), "delete");
    assert_eq!(how(&rules, &item(delete, Safety::Protected, 1)), "keep");
    let report = rule_with(&rules, "report");
    assert_eq!(how(&rules, &item(report, Safety::Review, 1)), "report only");
    let native = how(&rules, &item(rule_with(&rules, "native"), Safety::Safe, 1));
    assert!(
        native.starts_with("run `") && native.ends_with('`'),
        "{native}"
    );
}

/// The summary counts every match by safety level; its total and the recommended total leave
/// protected matches out.
#[test]
fn the_summary_counts_by_safety_and_leaves_protected_out_of_the_total() {
    let rules = rules();
    let rule = rule_with(&rules, "delete");
    let mut recommended = item(rule, Safety::Safe, 100);
    recommended.recommended = true;
    let items = [
        recommended,
        item(rule, Safety::Safe, 20),
        item(rule, Safety::Review, 5),
        item(rule, Safety::Protected, 1000),
    ];
    let s = summary(&items);
    assert_eq!(
        (s["items"].as_u64(), s["disk"].as_u64()),
        (Some(4), Some(125))
    );
    assert_eq!(s["by_safety"]["safe"], json!({ "items": 2, "disk": 120 }));
    assert_eq!(s["by_safety"]["review"], json!({ "items": 1, "disk": 5 }));
    assert_eq!(
        s["by_safety"]["protected"],
        json!({ "items": 1, "disk": 1000 })
    );
    assert_eq!(
        s["by_safety"]["regenerable"],
        json!({ "items": 0, "disk": 0 })
    );
    assert_eq!(s["recommended"], json!({ "items": 1, "disk": 100 }));
}

#[test]
fn an_empty_table_says_nothing_was_found() {
    assert_eq!(printed(&rules(), &[], Some(1), false), "nothing found\n");
}

/// Each row names its rule and path with what it is flagged with, and the totals follow: by
/// safety level, of the recommended matches, and against `--fail-above`.
#[test]
fn the_table_lists_each_match_with_its_flags_and_the_totals() {
    let rules = rules();
    let rule = rule_with(&rules, "delete");
    let mut a = item(rule, Safety::Safe, 2048);
    a.recommended = true;
    a.class.flags = ClassFlags::IN_USE | ClassFlags::VCS_TRACKED;
    a.matches_inside = 1;
    let mut b = item(rule, Safety::Review, 1024);
    b.class.flags = ClassFlags::VCS_UNKNOWN;
    b.matches_inside = 2;
    b.mtime = 3 * DAY;
    let out = printed(&rules, &[a, b], Some(1024), true);
    let lines: Vec<&str> = out.lines().collect();
    assert!(
        lines[0].starts_with("SAFETY") && lines[0].ends_with("PATH"),
        "{out}"
    );
    let name = &rules.rule(rule).name;
    assert!(
        lines[1].starts_with("safe") && lines[1].contains(name.as_str()),
        "{out}"
    );
    assert!(lines[1].contains("/home/u/x"), "{out}");
    let flags = "[recommended; in use; tracked by git; without the 1 match inside; delete]";
    assert!(lines[1].ends_with(flags), "{out}");
    assert!(lines[2].contains("    7d  "), "{out}");
    let flags = "[git tracking unknown; without the 2 matches inside; delete]";
    assert!(lines[2].ends_with(flags), "{out}");
    assert_eq!(lines[3], "");
    assert!(
        lines[4].starts_with("safe") && lines[4].ends_with("1 item"),
        "{out}"
    );
    assert!(
        lines[5].starts_with("review") && lines[5].ends_with("1 item"),
        "{out}"
    );
    let rec = "1 item (dfdu clean deletes these)";
    assert!(
        lines[6].starts_with("recommended") && lines[6].ends_with(rec),
        "{out}"
    );
    assert_eq!(lines[7], "3.0 KiB is over the limit (--fail-above 1.0 KiB)");
    assert_eq!(lines.len(), 8, "{out}");

    let within = printed(&rules, &[item(rule, Safety::Safe, 10)], Some(1024), false);
    assert!(
        within.ends_with("is within the limit (--fail-above 1.0 KiB)\n"),
        "{within}"
    );
    let no_limit = printed(&rules, &[item(rule, Safety::Safe, 10)], None, false);
    assert!(
        no_limit.ends_with("0 items (dfdu clean deletes these)\n"),
        "{no_limit}"
    );
}

/// An item's JSON names its rule and action, its sizes and flags; the native command and the
/// matches inside it only when there are.
#[test]
fn an_item_in_json_names_its_rule_its_sizes_and_its_flags() {
    let rules = rules();
    let rule = rule_with(&rules, "delete");
    let mut it = item(rule, Safety::Regenerable, 4096);
    it.class.flags = ClassFlags::CHECKED | ClassFlags::IN_USE;
    it.mtime = 3 * DAY;
    let j = item_json(&rules, &it, 10 * DAY);
    let r = rules.rule(rule);
    assert_eq!(j["path"], "/home/u/x");
    assert_eq!(
        (j["type"].as_str(), j["rule"].as_str()),
        (Some("dir"), Some(r.id.as_str()))
    );
    assert_eq!(
        (j["safety"].as_str(), j["action"].as_str()),
        (Some("regenerable"), Some("delete"))
    );
    assert_eq!(
        (j["disk"].as_u64(), j["apparent"].as_u64()),
        (Some(4096), Some(2048))
    );
    assert_eq!(
        (j["entries"].as_u64(), j["age_days"].as_i64()),
        (Some(3), Some(7))
    );
    let flags = [
        "checked",
        "in_use",
        "vcs_tracked",
        "recommended",
        "scanning",
    ];
    let flags = flags.map(|f| j[f].as_bool());
    assert_eq!(flags, [true, true, false, false, false].map(Some));
    assert!(j.get("native_command").is_none() && j.get("matches_inside").is_none());
    assert!(j.get("degraded").is_none());

    let native = rule_with(&rules, "native");
    let mut it = item(native, Safety::Safe, 1);
    it.kind = Kind::File;
    it.matches_inside = 2;
    let j = item_json(&rules, &it, 0);
    assert_eq!(j["type"], "file");
    assert!(
        j["native_command"]
            .as_array()
            .is_some_and(|a| !a.is_empty()),
        "{j}"
    );
    assert_eq!(j["matches_inside"], 2);
}

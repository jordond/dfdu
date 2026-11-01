use dfdu_engine::test_support::meta;

use super::*;
use crate::state::tests::{opened, sync};
use crate::test_support::{Fixture, classified, tree};

/// A browser of `m` with the cleanup view opened and read.
fn cleanup(m: &Model) -> SessionState {
    opened(m, View::Cleanup).0
}

/// [`classified`] with `/r/old-cache`, a file of 1 GiB unchanged for years under a safe rule:
/// the one recommended item.
fn with_recommended() -> Fixture {
    let mut fx = classified();
    let t = &mut fx.m.tree;
    let root = t.roots().next().expect("a root");
    let f = t.add_file(
        root,
        b"old-cache",
        &meta(Kind::File, 1 << 30, 900),
        false,
        None,
    );
    let own = t.own(f);
    t.propagate_add(root, &own);
    let rules = fx.m.rules.as_ref().expect("rules");
    let class = Class {
        rule: rules.index("generic.cachedir-tag").expect("the rule"),
        safety: Safety::Safe,
        degraded: None,
        flags: dfdu_tree::ClassFlags::NONE,
    };
    fx.m.tree.set_class(f, Some(class));
    fx
}

/// The safety of each section and group row, and the item's name for the rest.
fn shown(s: &SessionState) -> Vec<String> {
    (rows(s).iter())
        .map(|&r| match r {
            Row::Section(safety) => format!("{safety:?}"),
            Row::Group { safety, .. } => format!("  {safety:?}"),
            Row::Item(i) => {
                let path = &s.extras.cleanup.items[i].path;
                format!(
                    "    {}",
                    String::from_utf8_lossy(path.file_name().unwrap_or_default())
                )
            }
        })
        .collect()
}

fn marked(s: &SessionState) -> Vec<&HostPath> {
    s.marks.items().iter().map(|i| &i.target.path).collect()
}

#[test]
fn opening_asks_for_the_native_sizes() {
    let fx = classified();
    let (s, fx_out) = opened(&fx.m, View::Cleanup);
    assert!(
        matches!(fx_out.as_slice(), [Effect::Send(Command::NativeSizes)]),
        "{fx_out:?}"
    );
    assert!(active(&s));
    assert_eq!(s.extras.cleanup.items.len(), 2);
    // Both are below their rules' minimum sizes: nothing is recommended.
    assert!(s.marks.is_empty());
}

#[test]
fn sections_are_open_and_categories_closed_by_default() {
    let fx = classified();
    let mut s = cleanup(&fx.m);
    assert_eq!(
        shown(&s),
        ["Safe", "  Safe", "Regenerable", "  Regenerable"]
    );
    assert_eq!(enter_does(&s), "close", "on the open section");
    let group = rows(&s)[3];
    assert!(!is_open(&s, group));
    set_open(&mut s, group, true);
    assert!(is_open(&s, group));
    assert_eq!(
        shown(&s),
        ["Safe", "  Safe", "Regenerable", "  Regenerable", "    big"]
    );
    s.cleanup.cursor = 4;
    assert_eq!(enter_does(&s), "show in Files");
    let item = selected(&s).expect("the item selected");
    assert!(item.path.to_local().ends_with("big"));
    // A closed section hides its categories.
    set_open(&mut s, Row::Section(Safety::Safe), false);
    assert_eq!(shown(&s)[..2], ["Safe", "Regenerable"]);
    s.cleanup.cursor = 0;
    assert_eq!(enter_does(&s), "open");
    assert!(selected(&s).is_none());
}

#[test]
fn the_filter_keeps_the_safer_levels_and_items_go_by_group() {
    let fx = classified();
    let mut s = cleanup(&fx.m);
    let Row::Group { cat, safety } = rows(&s)[3] else {
        panic!("a group row");
    };
    assert_eq!(group_items(&s, cat, safety).len(), 1);
    assert_eq!(section_items(&s, Safety::Regenerable).len(), 1);
    assert_eq!(group_items(&s, cat, Safety::Safe), Vec::<usize>::new());
    s.cleanup.filter = Some(Safety::Safe);
    assert_eq!(shown(&s), ["Safe", "  Safe"]);
    // Without rules there is nothing to group.
    s.snap.rules = None;
    assert_eq!(rows(&s), []);
    assert_eq!(group_items(&s, cat, safety), Vec::<usize>::new());
}

#[test]
fn intents_open_close_tick_and_show_in_files() {
    let fx = classified();
    let mut s = cleanup(&fx.m);
    // The safe section's category opens, its item ticks, and on the category Toggle unticks
    // all of it.
    let _ = act(&mut s, Intent::Select(1));
    let _ = act(&mut s, Intent::Activate);
    let _ = act(&mut s, Intent::Select(2));
    let _ = act(&mut s, Intent::Toggle);
    assert!(marked(&s)[0].to_local().ends_with("mid/m0"));
    assert_eq!(s.cleanup.cursor, 3, "the cursor moves on past an item");
    let _ = act(&mut s, Intent::Select(1));
    let _ = act(&mut s, Intent::Toggle);
    assert!(s.marks.is_empty(), "a group toggles all of it");
    // On the section, the whole level; CloseUp from the item closes the category.
    let _ = act(&mut s, Intent::Select(0));
    let _ = act(&mut s, Intent::Toggle);
    assert_eq!(s.marks.items().len(), 1);
    let _ = act(&mut s, Intent::Select(2));
    let _ = act(&mut s, Intent::CloseUp);
    assert_eq!(s.cleanup.cursor, 1);
    assert_eq!(
        shown(&s),
        ["Safe", "  Safe", "Regenerable", "  Regenerable"]
    );
    // Activating an item asks to show it in Files, below the scan root.
    let _ = act(&mut s, Intent::Activate);
    let then = act(&mut s, Intent::Select(2));
    assert!(then.show.is_none() && then.effects.is_empty());
    let show = act(&mut s, Intent::Activate).show.expect("shown in Files");
    assert!(show.path.to_local().ends_with("mid/m0"));
    assert_eq!(show.root, s.snap.root_path);
    // The filter shows the safe items only.
    let _ = act(&mut s, Intent::CycleFilter);
    assert_eq!(s.status.as_deref(), Some("cleanup: showing safe only"));
    assert_eq!((s.cleanup.cursor, shown(&s).len()), (0, 3));
}

#[test]
fn recommended_items_are_marked_only_when_the_view_opens() {
    let fx = with_recommended();
    let mut s = SessionState::new(std::time::Instant::now());
    sync(&mut s, &fx.m);
    // On without the user opening the view: the read takes the matches in, and marks nothing.
    s.view = View::Cleanup;
    sync(&mut s, &fx.m);
    assert_eq!(s.extras.cleanup.items.len(), 3);
    assert!(s.marks.is_empty());
    s.view = View::Files;

    // Opening it marks the recommended item, and says so.
    s.view = View::Cleanup;
    let _ = on_open(&mut s, false);
    sync(&mut s, &fx.m);
    assert_eq!(marked(&s), [&HostPath::from("/r").join(b"old-cache")]);
    assert_eq!(
        s.status.as_deref(),
        Some("Ticked 1 suggested item (1.0 GiB) · X untick all")
    );
    // Unmarked, it stays so: through later reads, and when the view opens again.
    marks::clear(&mut s);
    sync(&mut s, &fx.m);
    s.view = View::Files;
    s.view = View::Cleanup;
    let _ = on_open(&mut s, false);
    sync(&mut s, &fx.m);
    assert!(s.marks.is_empty());
    // Ticking the suggested items asks for it again.
    let _ = act(&mut s, Intent::TickSuggested);
    assert_eq!(marked(&s).len(), 1);
    let _ = act(&mut s, Intent::TickSuggested);
    assert_eq!(
        s.status.as_deref(),
        Some("every suggested item is ticked already")
    );
}

#[test]
fn a_read_only_browser_never_marks() {
    let fx = with_recommended();
    let mut s = SessionState::new(std::time::Instant::now());
    s.read_only = true;
    sync(&mut s, &fx.m);
    s.view = View::Cleanup;
    let _ = on_open(&mut s, false);
    sync(&mut s, &fx.m);
    assert!(s.marks.is_empty());
    // Ticking says why instead.
    let _ = act(&mut s, Intent::TickSuggested);
    assert!(s.marks.is_empty());
    assert_eq!(s.status.as_deref(), Some(READ_ONLY));
    s.status = None;
    let _ = act(&mut s, Intent::Toggle);
    assert!(s.marks.is_empty());
    assert_eq!(s.status.as_deref(), Some(READ_ONLY));
}

#[test]
fn no_matches_list_nothing() {
    let mut fx = tree();
    let env = dfdu_rules::Env::with(dfdu_rules::Platform::Linux, &[("HOME", "/home/u")]);
    fx.m.rules = Some(std::sync::Arc::new(dfdu_rules::builtin(&env).0));
    let s = cleanup(&fx.m);
    assert!(active(&s) && s.extras.cleanup.items.is_empty());
    assert_eq!(rows(&s), []);
}

#[test]
fn recommended_items() {
    let fx = classified();
    let s = cleanup(&fx.m);
    let rules = s.snap.rules.clone().expect("rules");
    let base = s.extras.cleanup.items[0].clone();
    let rule = |id: &str| rules.index(id).expect("the rule");
    let now = now_secs();
    let day = 86_400;
    let it = |id: &str, safety: Safety, size: u64, age_days: i64, flags: u8| Item {
        class: Class {
            rule: rule(id),
            safety,
            flags: dfdu_tree::ClassFlags::from_bits(flags),
            ..base.class
        },
        size,
        mtime: now - age_days * day,
        ..base.clone()
    };
    let gib = 1 << 30;
    let v = |it: Item| recommended(&s, &it, now).expect("rules");
    // Safe above its minimum (50 MiB), quiet for a week.
    assert_eq!(
        v(it("generic.cachedir-tag", Safety::Safe, gib, 8, 0)),
        Verdict::Safe
    );
    assert_eq!(
        v(it("generic.cachedir-tag", Safety::Safe, 1 << 20, 8, 0)),
        Verdict::Below(50 << 20)
    );
    assert_eq!(
        v(it("generic.cachedir-tag", Safety::Safe, gib, 2, 0)),
        Verdict::Newer(7)
    );
    // Regenerable only when stale (the Rust rule: 14 days).
    assert_eq!(
        v(it("rust.target", Safety::Regenerable, gib, 15, 0)),
        Verdict::Stale(14)
    );
    assert_eq!(
        v(it("rust.target", Safety::Regenerable, gib, 10, 0)),
        Verdict::Newer(14)
    );
    // Never review, nothing in use or tracked by git.
    assert_eq!(
        v(it("rust.target", Safety::Review, gib, 99, 0)),
        Verdict::Level(Safety::Review)
    );
    let in_use = dfdu_tree::ClassFlags::IN_USE.bits();
    assert_eq!(
        v(it("rust.target", Safety::Regenerable, gib, 99, in_use)),
        Verdict::InUse
    );
    assert!(Verdict::Safe.recommended() && Verdict::Stale(14).recommended());
    assert!(!Verdict::Newer(7).recommended());
}

/// The user caches yield to the caches inside them: listed for the rest, and marking them
/// marks that rest, not the folder whole.
#[test]
fn marking_a_container_marks_its_rest() {
    let fx = dfdu_engine::test_support::caches();
    let cache = fx.find("Caches");
    let m = Model {
        tree: fx.tree,
        rules: Some(std::sync::Arc::new(fx.rules)),
        ..Model::default()
    };
    let mut s = cleanup(&m);
    let items = s.extras.cleanup.items.clone();
    let at = items
        .iter()
        .position(|it| it.path.to_local().ends_with("Caches"))
        .expect("the user caches are listed");
    let it = &items[at];
    assert_eq!(it.size, m.tree.total(cache).alloc - 500);
    let mut rest: Vec<&HostPath> = it.rest.iter().map(|r| &r.path).collect();
    rest.sort();
    assert_eq!(rest.len(), 2, "{rest:?}");
    assert!(
        items
            .iter()
            .any(|i| i.path.to_local().ends_with("Caches/pip"))
    );
    let rules = s.snap.rules.clone().expect("rules");
    let cat = rules.rule(it.class.rule).category;
    s.cleanup.open.insert((cat, it.class.safety));
    let row = rows(&s).iter().position(|r| *r == Row::Item(at));
    let _ = act(&mut s, Intent::Select(row.expect("the item's row")));
    let _ = act(&mut s, Intent::Toggle);
    let mut got = marked(&s);
    got.sort();
    assert_eq!(got, rest);
    assert!(is_marked(&s, it));
    s.cleanup.cursor -= 1;
    let _ = act(&mut s, Intent::Toggle);
    assert!(s.marks.is_empty());
}

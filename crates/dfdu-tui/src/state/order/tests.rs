use super::*;
use crate::model::snapshot::test_entry;

fn names(o: &Order, entries: &[EntryData]) -> Vec<String> {
    o.rows()
        .iter()
        .map(|&i| String::from_utf8_lossy(&entries[i].name).into_owned())
        .collect()
}

fn all(_: usize, _: &EntryData) -> bool {
    true
}

#[test]
fn exact_sort_by_size_and_name() {
    let es = vec![
        test_entry(0, "b", 10),
        test_entry(1, "a", 30),
        test_entry(2, "c", 20),
    ];
    let mut o = Order::default();
    o.sort_exact(&es, all, SortSpec::default());
    assert_eq!(names(&o, &es), ["a", "c", "b"]);
    o.sort_exact(&es, all, SortSpec::for_key(SortKey::Name));
    assert_eq!(names(&o, &es), ["a", "b", "c"]);
}

#[test]
fn rows_keep_positions_between_sorts_and_new_entries_wait() {
    let mut es = vec![test_entry(0, "a", 100), test_entry(1, "b", 50)];
    let mut o = Order::default();
    o.sync(&es, all, SortSpec::default());
    assert_eq!(names(&o, &es), ["a", "b"]);
    // "b" grows past "a" and a new entry arrives: nothing moves until a sort.
    es[1].size = 500;
    es.push(test_entry(2, "c", 1000));
    o.sync(&es, all, SortSpec::default());
    assert_eq!(names(&o, &es), ["a", "b"]);
    assert_eq!(o.new_entries().len(), 1);
    o.sort_auto(&es, all, SortSpec::default());
    assert_eq!(names(&o, &es), ["c", "b", "a"]);
    assert_eq!(o.new_entries(), [] as [usize; 0]);
}

#[test]
fn auto_sort_does_not_swap_nearly_equal_siblings() {
    let mut es = vec![test_entry(0, "x", 1_000_000), test_entry(1, "y", 1_001_000)];
    let mut o = Order::default();
    o.sort_exact(&es, all, SortSpec::default());
    assert_eq!(names(&o, &es), ["y", "x"]);
    // "x" overtakes "y" by less than 2%: an automatic sort keeps the previous order...
    es[0].size = 1_003_000;
    assert_eq!(bucket(es[0].size), bucket(es[1].size));
    o.sort_auto(&es, all, SortSpec::default());
    assert_eq!(names(&o, &es), ["y", "x"]);
    // ...an explicit sort does not.
    o.sort_exact(&es, all, SortSpec::default());
    assert_eq!(names(&o, &es), ["x", "y"]);
}

#[test]
fn vanished_rows_are_dropped() {
    let es = vec![
        test_entry(0, "a", 3),
        test_entry(1, "b", 2),
        test_entry(2, "c", 1),
    ];
    let mut o = Order::default();
    o.sync(&es, all, SortSpec::default());
    let fewer = vec![es[0].clone(), es[2].clone()];
    o.remap(|i| [Some(0), None, Some(1)][i]);
    o.sync(&fewer, all, SortSpec::default());
    assert_eq!(names(&o, &fewer), ["a", "c"]);
}

/// Entries with repeated sizes, counts, times and names that differ only in case or after a
/// long shared prefix, so that every tie-break of the exact sort takes part.
fn ties(n: u32) -> Vec<EntryData> {
    let names = [
        "a",
        "A",
        "same-prefix-for-sixteen-a",
        "Same-prefix-for-sixteen-a",
        ".h",
    ];
    (0..n)
        .map(|i| {
            let name = format!("{}{}", names[(i * 7 % 5) as usize], i % 3);
            let mut e = test_entry(i, &name, [0, 10, 1000, 1001][(i * 3 % 4) as usize]);
            e.items = u64::from(i % 4);
            e.mtime = i64::from(i % 3) - 1;
            e
        })
        .collect()
}

/// A changed filter picks its rows from the kept exact sort, and gets the rows an exact sort
/// of the entries it passes gives, whether it narrows, widens or is cleared.
#[test]
fn refilter_gives_the_rows_of_an_exact_sort() {
    let es = ties(200);
    let shown = |_: usize, e: &EntryData| !e.hidden();
    let filters: [fn(usize) -> bool; 5] = [
        |i| i.is_multiple_of(2),
        |i| i.is_multiple_of(6),
        |i| i % 3 != 1,
        |_| false,
        |_| true,
    ];
    for key in [SortKey::Size, SortKey::Name, SortKey::Items, SortKey::Mtime] {
        for desc in [false, true] {
            let spec = SortSpec { key, desc };
            let mut o = Order::default();
            o.sort_exact_unfiltered(&es, shown, spec);
            for f in filters {
                let visible = |i: usize, e: &EntryData| shown(i, e) && f(i);
                assert!(o.refilter(&es, visible, spec));
                let mut want = Order::default();
                want.sort_exact(&es, visible, spec);
                assert_eq!(o.rows, want.rows, "{spec:?}");
            }
            // A sort kept apart from the rows gives the same rows.
            let mut o = Order::default();
            o.sort_auto(&es, all, spec);
            let auto = o.rows.clone();
            o.keep_exact(&es, shown, spec);
            assert_eq!(o.rows, auto, "keeping a sort leaves the rows alone");
            let visible = |i: usize, e: &EntryData| shown(i, e) && i.is_multiple_of(2);
            assert!(o.refilter(&es, visible, spec));
            let mut want = Order::default();
            want.sort_exact(&es, visible, spec);
            assert_eq!(o.rows, want.rows, "{spec:?}");
        }
    }
}

/// Without an exact sort that still holds for the entries, `refilter` changes nothing, so
/// the rows are sorted instead.
#[test]
fn refilter_needs_an_exact_sort_that_still_holds() {
    let mut es = ties(20);
    let spec = SortSpec::default();
    let refilters = |o: &mut Order, es: &[EntryData]| {
        let rows = o.rows.clone();
        let done = o.refilter(es, |i, _| i.is_multiple_of(2), spec);
        assert!(done || o.rows == rows);
        done
    };
    let mut o = Order::default();
    assert!(!refilters(&mut o, &es), "no sort yet");
    o.sort_exact(&es, |i, _| i.is_multiple_of(3), spec);
    assert!(!refilters(&mut o, &es), "the rows were filtered");
    o.sort_exact_unfiltered(&es, all, SortSpec::for_key(SortKey::Name));
    assert!(!refilters(&mut o, &es), "sorted by another key");
    o.sort_exact_unfiltered(&es, all, spec);
    o.sort_auto(&es, all, spec);
    assert!(!refilters(&mut o, &es), "sorted automatically");
    o.sort_exact_unfiltered(&es, all, spec);
    es[3].size = 1 << 30;
    es.push(test_entry(20, "late", 5));
    o.sync(&es, all, spec);
    assert!(!refilters(&mut o, &es), "sizes and entries changed");
    o.keep_exact(&es, all, spec);
    o.remap(Some);
    assert!(!refilters(&mut o, &es), "the entries were replaced");
    o.keep_exact(&es, all, spec);
    assert!(refilters(&mut o, &es));
    assert!(
        refilters(&mut o, &es),
        "the kept sort stays for the next change"
    );
}

/// The sort on integer keys gives the order of a stable sort with the plain comparison,
/// including names that share long prefixes or differ only in case, and equal sizes.
#[test]
fn integer_keys_match_the_plain_comparison() {
    let names = [
        "a",
        "A",
        "b",
        "B",
        "same-prefix-for-sixteen-a",
        "same-prefix-for-sixteen-B",
        "Same-prefix-for-sixteen-b",
        "same-prefix-for-sixteen",
        "z",
        "Zz",
        ".hidden",
        "",
    ];
    let mut seed = 7u64;
    let mut next = |n: u64| {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (seed >> 33) % n
    };
    let mut es = Vec::new();
    for i in 0..300u32 {
        let name = format!(
            "{}{}",
            names[usize::try_from(next(names.len() as u64)).unwrap()],
            next(3)
        );
        let mut e = test_entry(
            i,
            &name,
            [0, 10, 1000, 1001, 5000][usize::try_from(next(5)).unwrap()],
        );
        e.items = next(4);
        e.mtime = i64::try_from(next(3)).unwrap() - 1;
        es.push(e);
    }
    for key in [SortKey::Size, SortKey::Name, SortKey::Items, SortKey::Mtime] {
        for desc in [false, true] {
            let spec = SortSpec { key, desc };
            for quantise in [false, true] {
                // A previous order for the automatic sort's tie-break.
                let prev: Vec<usize> = (0..es.len()).rev().filter(|i| i % 3 != 0).collect();
                let mut o = Order {
                    rows: prev.clone(),
                    new: Vec::new(),
                    base: Base::None,
                };
                o.sort(&es, all, spec, quantise);
                let pos = |i: usize| prev.iter().position(|&p| p == i).unwrap_or(usize::MAX);
                let value = |e: &EntryData| -> i64 {
                    let v = match key {
                        SortKey::Size => e.size,
                        SortKey::Items => e.items,
                        SortKey::Mtime => return e.mtime,
                        SortKey::Name => return 0,
                    };
                    if quantise {
                        bucket(v)
                    } else {
                        i64::try_from(v).unwrap()
                    }
                };
                let mut want: Vec<usize> = (0..es.len()).collect();
                want.sort_by(|&a, &b| {
                    let (x, y) = (&es[a], &es[b]);
                    let p = match key {
                        SortKey::Name => name_cmp(&x.name, &y.name),
                        SortKey::Mtime => x.mtime.cmp(&y.mtime),
                        _ => value(x).cmp(&value(y)),
                    };
                    let p = if desc { p.reverse() } else { p };
                    let keep = if quantise {
                        pos(a).cmp(&pos(b))
                    } else {
                        Ordering::Equal
                    };
                    p.then(keep).then_with(|| name_cmp(&x.name, &y.name))
                });
                assert_eq!(o.rows, want, "{spec:?} quantise {quantise}");
            }
        }
    }
}

#[test]
fn sort_top_sorts_the_first_rows_and_keeps_the_rest_in_place() {
    let mut es = vec![
        test_entry(0, "a", 500),
        test_entry(1, "b", 400),
        test_entry(2, "c", 300),
        test_entry(3, "d", 200),
    ];
    let mut o = Order::default();
    o.sort_exact(&es, all, SortSpec::default());
    // d grows past everything and two entries arrive, one large and one small.
    es[3].size = 2000;
    es.push(test_entry(4, "x", 1000));
    es.push(test_entry(5, "y", 1));
    o.sort_top(&es, all, SortSpec::default(), 2);
    // The first two rows are the two largest; the others keep their order after them, and the
    // small new entry waits for a sort.
    assert_eq!(names(&o, &es), ["d", "x", "a", "b", "c"]);
    assert_eq!(o.new_entries(), [5]);
    // c passes d by less than 2%: as in an automatic sort, d stays first.
    es[2].size = 2005;
    assert_eq!(bucket(es[2].size), bucket(es[3].size));
    o.sort_top(&es, all, SortSpec::default(), 2);
    assert_eq!(names(&o, &es), ["d", "c", "x", "a", "b"]);
}

/// The first rows of [`Order::sort_top`] are those of an automatic sort, for every key, with
/// names that share long prefixes and equal sizes; the other rows and new entries keep their
/// order.
#[test]
fn sort_top_matches_the_automatic_sort() {
    let es = ties(60);
    let n = es.len();
    // Rows in a scrambled order, a few new entries, and some not seen yet.
    let rows: Vec<usize> = (0..n).filter(|i| i % 5 != 0).rev().collect();
    let new: Vec<usize> = (0..n).filter(|i| i % 10 == 0).collect();
    let listed = |o: &Order| -> Vec<usize> { o.rows.iter().chain(&o.new).copied().collect() };
    for key in [SortKey::Size, SortKey::Name, SortKey::Items, SortKey::Mtime] {
        for desc in [false, true] {
            let spec = SortSpec { key, desc };
            for height in [0, 1, 2, 7, 20, n, n + 5] {
                let order = || Order {
                    rows: rows.clone(),
                    new: new.clone(),
                    base: Base::None,
                };
                let mut auto = order();
                auto.sync(&es, all, spec);
                let before = listed(&auto);
                auto.sort_auto(&es, all, spec);
                let mut top = order();
                top.sort_top(&es, all, spec, height);
                let h = height.min(n);
                let what = format!("{spec:?} height {height}");
                assert_eq!(top.rows[..h], auto.rows[..h], "{what}");
                let picked = &top.rows[..h];
                let rest: Vec<usize> = before
                    .iter()
                    .copied()
                    .filter(|i| !picked.contains(i))
                    .collect();
                assert_eq!(listed(&top)[h..], rest[..], "{what}");
            }
        }
    }
}

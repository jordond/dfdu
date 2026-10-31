use super::*;
use crate::test_util::small;

fn of(style: Style, files: usize, size: u64) -> Fill {
    Fill {
        files,
        size: Amount::Bytes(size),
        seed: 0,
        style,
        spread: None,
    }
}

/// Every file below `dir`: its path relative to `dir`, size and mtime.
fn files(t: &Tree, dir: usize) -> Vec<(String, u64, i64)> {
    let mut out = vec![];
    let mut stack = vec![(dir, String::new())];
    while let Some((d, at)) = stack.pop() {
        for e in t.list(d).unwrap() {
            let path = format!("{at}{}", String::from_utf8_lossy(&e.name));
            if t.nodes[e.node].is_dir() {
                stack.push((e.node, format!("{path}/")));
            } else {
                out.push((path, t.size(e.node), t.nodes[e.node].mtime));
            }
        }
    }
    out.sort();
    out
}

fn filled(f: &Fill) -> (Tree, usize) {
    let mut t = Tree::new(0, (0, 0));
    let dir = t.insert(0, b"d", Kind::dir(), 0, 0o755).unwrap();
    fill(&mut t, dir, f, 1_000_000_000, "d").unwrap();
    (t, dir)
}

#[test]
fn fill_makes_its_files_adding_up_to_its_size_the_same_every_time() {
    let t = small();
    let nm = t.resolve("dev/shop/node_modules").unwrap();
    let made = files(&t, nm);
    assert_eq!(made.len(), 50);
    assert_eq!(made.iter().map(|f| f.1).sum::<u64>(), 10 << 20);
    assert!(
        t.resolve("dev/shop/node_modules/react/package.json")
            .is_ok()
    );
    assert_eq!(made, files(&small(), nm));
}

#[test]
fn ages_fall_within_the_spread() {
    let mut f = of(Style::Generic, 200, 1 << 20);
    f.spread = Some("1h".into());
    let (t, dir) = filled(&f);
    let made = files(&t, dir);
    assert_eq!(made.len(), 200);
    assert!(
        made.iter()
            .all(|(_, _, m)| (1_000_000_000 - 3600..=1_000_000_000).contains(m))
    );
    assert!(made.iter().any(|(_, _, m)| *m != made[0].2), "ages vary");
    // The default spread is 30 days.
    let (t, dir) = filled(&of(Style::Generic, 200, 1 << 20));
    let oldest = files(&t, dir).iter().map(|f| f.2).min().unwrap();
    assert!(oldest >= 1_000_000_000 - 30 * 86_400);
    assert!(oldest < 1_000_000_000 - 3600);
}

#[test]
fn a_few_large_files_outweigh_many_small_ones() {
    let (t, dir) = filled(&of(Style::Generic, 100, 1 << 30));
    let mut sizes: Vec<u64> = files(&t, dir).iter().map(|f| f.1).collect();
    sizes.sort_unstable();
    assert_eq!(sizes.iter().sum::<u64>(), 1 << 30);
    let (smallest_half, largest_tenth): (u64, u64) =
        (sizes[..50].iter().sum(), sizes[90..].iter().sum());
    assert!(
        largest_tenth > 2 * smallest_half,
        "{largest_tenth} vs {smallest_half}"
    );
}

#[test]
fn the_seed_changes_sizes_but_not_names() {
    let a = of(Style::Generic, 20, 1 << 20);
    let mut b = of(Style::Generic, 20, 1 << 20);
    b.seed = 9;
    let (ta, da) = filled(&a);
    let (tb, db) = filled(&b);
    let (fa, fb) = (files(&ta, da), files(&tb, db));
    let names = |f: &[(String, u64, i64)]| f.iter().map(|f| f.0.clone()).collect::<Vec<_>>();
    assert_eq!(names(&fa), names(&fb));
    assert_ne!(fa, fb);
}

#[test]
fn each_style_lays_out_its_own_names() {
    let names = |style, files| {
        let mut rng = 1;
        layout(&of(style, files, 0), &mut rng)
    };
    assert_eq!(names(Style::Generic, 2), ["file-0001.dat", "file-0002.dat"]);
    let photos = names(Style::Photos, 8);
    assert_eq!(photos[0], "IMG_4000.HEIC");
    assert_eq!(photos[6], "IMG_4006.MOV");
    for blob in names(Style::Cache, 5) {
        let (dir, rest) = blob.split_once('/').unwrap();
        assert_eq!((dir.len(), rest.len()), (2, 14), "{blob}");
    }
    let npm = names(Style::Npm, NPM.len() * (NPM_FILES.len() + 1));
    assert_eq!(npm[0], "react/package.json");
    assert_eq!(npm[NPM.len() + 1], "react-dom/index.js");
    assert_eq!(
        npm.last().unwrap(),
        &format!("{}/lib/chunk-8.js", NPM[NPM.len() - 1])
    );
    let cargo = names(Style::Cargo, CRATES.len() * 4);
    let ext = |name: &str| name.rsplit_once('.').unwrap().1.to_owned();
    assert!(cargo[0].starts_with("deps/libserde-"));
    assert_eq!(ext(&cargo[0]), "rlib");
    assert_eq!(ext(&cargo[CRATES.len()]), "rmeta");
    assert_eq!(ext(&cargo[2 * CRATES.len()]), "d");
    assert!(cargo[3 * CRATES.len()].starts_with("incremental/serde-"));
    let python = names(Style::Python, PYTHON.len() * 4);
    assert_eq!(python[0], "numpy/__init__.py");
    assert_eq!(ext(&python[PYTHON.len()]), "so");
    assert!(python[2 * PYTHON.len()].ends_with(".dist-info/RECORD"));
    assert_eq!(python[3 * PYTHON.len()], "numpy/_mod3.py");
    // Names do not repeat, so every file is made.
    for style in [
        Style::Npm,
        Style::Cargo,
        Style::Python,
        Style::Cache,
        Style::Photos,
    ] {
        let mut all = names(style, 300);
        all.sort();
        all.dedup();
        assert_eq!(all.len(), 300, "{style:?}");
    }
}

#[test]
fn a_fill_into_a_file_is_refused() {
    let mut t = Tree::new(0, (0, 0));
    let dir = t.insert(0, b"d", Kind::dir(), 0, 0o755).unwrap();
    t.insert(dir, b"react", Kind::seeded(1, 1), 0, 0o644)
        .unwrap();
    let e = fill(&mut t, dir, &of(Style::Npm, 1, 10), 0, "d").unwrap_err();
    assert!(format!("{e:#}").contains("not a directory"), "{e:#}");
    let mut bad = of(Style::Generic, 1, 10);
    bad.spread = Some("a while".into());
    assert!(fill(&mut t, dir, &bad, 0, "d").is_err());
}

#[test]
fn reads_from_a_fixture() {
    let f: Fill =
        toml::from_str("files = 3\nsize = \"1KiB\"\nstyle = \"photos\"\nseed = 2").unwrap();
    assert_eq!((f.files, f.seed, f.size.bytes().unwrap()), (3, 2, 1024));
    assert!(matches!(f.style, Style::Photos));
    assert!(toml::from_str::<Fill>("files = 3\nsize = 1\nstyle = \"tar\"").is_err());
    assert!(toml::from_str::<Fill>("files = 3\nsize = 1\nextra = 1").is_err());
}

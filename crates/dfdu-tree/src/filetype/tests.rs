use super::*;
use crate::Meta;

#[test]
fn extensions() {
    assert_eq!(Ext::of(b"Trip.MOV").unwrap().as_str(), "mov");
    assert_eq!(Ext::of(b"src.tar.gz").unwrap().as_str(), "gz");
    assert!(Ext::of(b".bashrc").is_none());
    assert!(Ext::of(b"notes.").is_none());
    assert!(Ext::of(b"Makefile").is_none());
    assert!(Ext::of(b"x.backup-2023").is_none());
    assert!(Ext::of(b"x.abcdefghijk").is_none());
    assert_eq!(type_of(b"a.HEIC"), FileType::Image);
    assert_eq!(type_of(b"a.flac"), FileType::Audio);
    assert_eq!(type_of(b"Ubuntu.iso"), FileType::DiskImage);
    assert_eq!(type_of(b"README"), FileType::Other);
    assert_eq!(type_of(b"a.xyz"), FileType::Other);
}

#[test]
fn every_type_has_a_name_that_parses_back() {
    for t in FileType::ALL {
        assert_eq!(FileType::parse(t.name()), Some(t));
    }
    assert_eq!(FileType::parse("Videos"), Some(FileType::Video));
    assert_eq!(FileType::parse("pictures"), Some(FileType::Image));
    assert_eq!(FileType::parse("s"), None);
}

#[test]
fn filters() {
    let f = TypeFilter::parse("video, .iso pdf").unwrap();
    assert!(f.matches(b"a.mkv"));
    assert!(f.matches(b"b.ISO"));
    assert!(f.matches(b"c.pdf"));
    assert!(!f.matches(b"d.epub"));
    assert!(!f.matches(b"README"));
    assert_eq!(f.to_string(), "video, .iso, .pdf");
    // A type name with a dot is an extension.
    let f = TypeFilter::parse(".data").unwrap();
    assert!(f.matches(b"x.data") && !f.matches(b"x.csv"));
    let err = TypeFilter::parse("a/b").unwrap_err();
    assert_eq!(
        err.to_string(),
        "`a/b` is neither a file type nor an extension"
    );
    assert!(TypeFilter::default().matches(b"anything"));
    let other = TypeFilter::parse("other").unwrap();
    assert!(other.matches(b"README") && other.matches(b"a.xyz") && !other.matches(b"a.mp3"));
}

#[test]
fn sums_count_matching_files_below() {
    let meta = |kind, alloc| Meta {
        kind,
        apparent: alloc,
        alloc,
        mtime: 0,
        dev: 1,
        ino: alloc,
        nlink: 1,
        mount: 0,
    };
    let mut t = Tree::new();
    let root = t.new_root(b"/r", &meta(Kind::Dir, 0), true);
    let sub = t.add_dir(root, b"sub", &meta(Kind::Dir, 0));
    t.add_file(root, b"a.mp4", &meta(Kind::File, 100), false, None);
    t.add_file(root, b"b.txt", &meta(Kind::File, 10), false, None);
    t.add_file(sub, b"c.MKV", &meta(Kind::File, 1000), false, None);
    t.add_file(sub, b"d.mp3", &meta(Kind::File, 50), false, None);
    let video = TypeFilter::parse("video").unwrap();
    let mut b = Breakdown::default();
    let s = sum(&t, root, &video, Some(&mut b));
    assert_eq!((s.alloc, s.items), (1100, 2));
    assert_eq!(sum(&t, sub, &video, None).alloc, 1000);
    let all = sum(&t, root, &TypeFilter::default(), None);
    assert_eq!((all.alloc, all.items), (1160, 4));
    assert_eq!(b.by_type[FileType::Video as usize].alloc, 1100);
    assert_eq!(b.by_type[FileType::Audio as usize].alloc, 0);
    let exts = b.exts_by(|t| t.alloc);
    assert_eq!(exts[0].0.as_str(), "mkv");
    assert_eq!(exts[1].0.as_str(), "mp4");
}

/// Cost of the walk the browser does per snapshot while a type filter is on (and once more
/// for the picker's breakdown), on 1M entries in 10k directories.
/// `cargo test --release -p dfdu-tree sum_cost -- --ignored --nocapture`.
#[test]
#[ignore = "benchmark: run in release with --nocapture"]
fn sum_cost() {
    const NAMES: [&str; 8] = [
        "a.mp4", "b.txt", "c.rs", "d.jpg", "Makefile", "e.tar.gz", "f.json", "g.o",
    ];
    let meta = |kind| Meta {
        kind,
        apparent: 1000,
        alloc: 4096,
        mtime: 0,
        dev: 1,
        ino: 0,
        nlink: 1,
        mount: 0,
    };
    let mut t = Tree::new();
    let root = t.new_root(b"/r", &meta(Kind::Dir), true);
    for d in 0..100 {
        let top = t.add_dir(root, format!("d{d}").as_bytes(), &meta(Kind::Dir));
        for s in 0..100 {
            let sub = t.add_dir(top, format!("s{s}").as_bytes(), &meta(Kind::Dir));
            for f in 0..100 {
                let n = format!("{f}{}", NAMES[f % NAMES.len()]);
                t.add_file(sub, n.as_bytes(), &meta(Kind::File), false, None);
            }
        }
    }
    let video = TypeFilter::parse("video,.json").unwrap();
    for (what, f, b) in [
        ("filter", &video, false),
        ("breakdown", &TypeFilter::default(), true),
    ] {
        let mut best = std::time::Duration::MAX;
        for _ in 0..5 {
            let mut bd = Breakdown::default();
            let start = std::time::Instant::now();
            let s = sum(&t, root, f, b.then_some(&mut bd));
            best = best.min(start.elapsed());
            assert!(s.items > 0);
        }
        eprintln!("{what}: {best:?} for 1M entries");
    }
}

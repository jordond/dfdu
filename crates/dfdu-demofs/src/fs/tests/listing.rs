//! Directory listings and where a resumed one starts.

use super::super::*;
use crate::test_support::small;

#[test]
fn a_listing_resumes_after_its_offset() {
    let mut t = small();
    let shop = t.resolve("dev/shop").unwrap();
    let all = listing(&t, shop, 0).unwrap();
    let names: Vec<_> = all.iter().map(|l| l.name).collect();
    assert_eq!(
        names,
        [
            &b"."[..],
            b"..",
            b"package.json",
            b"node_modules",
            b"link",
            b"hard"
        ]
    );
    assert_eq!((all[0].cookie, all[0].ino), (1, ino(shop)));
    assert_eq!((all[1].cookie, all[1].ino), (2, ino(t.parent(shop))));
    let kinds: Vec<_> = all.iter().map(|l| l.kind).collect();
    assert_eq!(
        kinds,
        [
            FileType::Directory,
            FileType::Directory,
            FileType::RegularFile,
            FileType::Directory,
            FileType::Symlink,
            FileType::RegularFile,
        ]
    );
    let after_dots = listing(&t, shop, 2).unwrap();
    assert_eq!(after_dots, listing(&t, shop, 0).unwrap()[2..]);
    // Resumed after an entry that has since gone, nothing is skipped or repeated.
    let resume = all[3].cookie;
    let rest: Vec<_> = all[4..].iter().map(|l| l.name.to_vec()).collect();
    t.unlink(shop, b"package.json", 0).unwrap();
    let pkg = t.resolve("dev/shop/hard").unwrap();
    t.rename(shop, b"hard", shop, b"hard2", false, false, 0)
        .unwrap();
    let names: Vec<_> = listing(&t, shop, resume)
        .unwrap()
        .iter()
        .map(|l| l.name.to_vec())
        .collect();
    assert_eq!(names, [rest[0].clone(), b"hard2".to_vec()]);
    assert_eq!(t.resolve("dev/shop/hard2"), Ok(pkg));
    assert_eq!(listing(&t, shop, u64::MAX).unwrap().len(), 0);
    assert_eq!(listing(&t, pkg, 0), Err(FsError::NotDir));
}

#[test]
fn the_root_lists_itself_as_its_parent() {
    let t = small();
    let root = listing(&t, 0, 0).unwrap();
    assert_eq!((root[0].ino, root[1].ino), (INodeNo(1), INodeNo(1)));
}

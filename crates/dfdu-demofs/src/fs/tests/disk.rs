//! The disk and the clock a mount serves from its fixture.

use super::super::*;
use crate::test_support::SMALL;

#[test]
fn free_space_grows_as_files_go_and_stays_within_the_disk() {
    let f = Fixture::parse(SMALL).unwrap();
    let fs = DemoFs::new(&f, f.build().unwrap()).unwrap();
    assert_eq!(fs.total, 1_000_000_000_000);
    assert_eq!(fs.free(&fs.tree()), 200_000_000_000);
    {
        let mut t = fs.tree();
        let dl = t.resolve("Downloads").unwrap();
        t.unlink(dl, b"b.iso", 0).unwrap();
    }
    assert_eq!(fs.free(&fs.tree()), 200_000_000_000 + (1 << 20));
    // No more than the disk, however much a fixture says is free.
    let f =
        Fixture::parse("now = \"2026-10-01\"\n[disk]\ntotal = \"1GB\"\nfree = \"2GB\"").unwrap();
    let fs = DemoFs::new(&f, f.build().unwrap()).unwrap();
    assert_eq!(fs.free(&fs.tree()), 1_000_000_000);
}

#[test]
fn the_clock_starts_at_the_fixtures_now() {
    let f = Fixture::parse(SMALL).unwrap();
    let fs = DemoFs::new(&f, f.build().unwrap()).unwrap();
    let now = f.now().unwrap();
    assert!((now..now + 5).contains(&fs.time()));
}

#[test]
fn a_disk_that_is_not_a_size_is_refused() {
    let f = Fixture::parse("now = \"2026-10-01\"\n[disk]\ntotal = \"big\"").unwrap();
    let e = DemoFs::new(&f, Tree::new(0, (0, 0))).err().unwrap();
    assert!(format!("{e:#}").contains("disk total"), "{e:#}");
}

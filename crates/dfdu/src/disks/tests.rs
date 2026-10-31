use dfdu_sys::{Disk, FsClass, Space};

use super::*;

fn disk(at: &str, purgeable: Option<u64>) -> Disk {
    Disk {
        mount_point: at.into(),
        also_at: vec![],
        fstype: "apfs".into(),
        source: String::new(),
        class: FsClass::Local,
        space: Some(Space {
            total: 1000 << 20,
            used: 600 << 20,
            available: 400 << 20,
            purgeable,
        }),
        role: None,
        shares_space_with: vec![],
    }
}

fn text(list: &[Disk]) -> String {
    let mut out = vec![];
    table(&mut out, list).unwrap();
    String::from_utf8(out).unwrap()
}

/// The PURGEABLE column is there only when some disk has purgeable space; a disk without
/// shows `-` in it.
#[test]
fn purgeable_column_only_when_some_disk_has_it() {
    let without = text(&[disk("/", None)]);
    assert!(!without.contains("PURGEABLE"), "{without}");
    let with = text(&[disk("/", Some(300 << 20)), disk("/Volumes/USB", None)]);
    let lines: Vec<&str> = with.lines().collect();
    assert!(lines[0].contains("FREE  PURGEABLE  USE%"), "{with}");
    let purgeable = dfdu_engine::human(300 << 20);
    assert!(lines[1].contains(&format!("{purgeable}   60%")), "{with}");
    assert!(lines[2].contains(" -   60%"), "{with}");
}

use super::*;

/// The identity `verify` compares, as the scanner records it.
fn id(p: &Path) -> (u64, u64) {
    let m = dfdu_tree::Meta::from_windows(&dfdu_sys::stat_path(p, false).unwrap());
    (m.dev, m.ino)
}

fn junction(link: &Path, target: &Path) {
    let ok = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()
        .unwrap();
    assert!(ok.status.success(), "{ok:?}");
}

/// The item and everything in it go, read-only files too; a junction inside is removed as
/// a link, and what it points at stays.
#[test]
fn removes_the_checked_item() {
    let tmp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("keep"), b"k").unwrap();
    let d = tmp.path().join("d");
    std::fs::create_dir_all(d.join("a/b")).unwrap();
    std::fs::write(d.join("a/b/f"), b"x").unwrap();
    let ro = d.join("ro");
    std::fs::write(&ro, b"x").unwrap();
    let mut perm = std::fs::metadata(&ro).unwrap().permissions();
    perm.set_readonly(true);
    std::fs::set_permissions(&ro, perm).unwrap();
    junction(&d.join("j"), outside.path());
    let (dev, ino) = id(&d);
    remove_verified(&d, Kind::Dir, dev, ino).unwrap();
    assert!(!d.exists());
    assert!(outside.path().join("keep").exists(), "followed a junction");
    let f = tmp.path().join("f");
    std::fs::write(&f, b"x").unwrap();
    let mut perm = std::fs::metadata(&f).unwrap().permissions();
    perm.set_readonly(true);
    std::fs::set_permissions(&f, perm).unwrap();
    let (dev, ino) = id(&f);
    remove_verified(&f, Kind::File, dev, ino).unwrap();
    assert!(!f.exists());
}

/// A directory above the item replaced by a junction after the item was checked: the path
/// now names another directory, which is not deleted.
#[test]
fn a_swapped_path_deletes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let item = tmp.path().join("a/b/item");
    std::fs::create_dir_all(&item).unwrap();
    std::fs::write(tmp.path().join("a/b/file"), b"x").unwrap();
    let (dev, ino) = id(&item);
    let (fdev, fino) = id(&tmp.path().join("a/b/file"));
    let other = tmp.path().join("other");
    std::fs::create_dir_all(other.join("b/item")).unwrap();
    std::fs::write(other.join("b/item/precious"), b"p").unwrap();
    std::fs::write(other.join("b/file"), b"p").unwrap();
    std::fs::rename(tmp.path().join("a"), tmp.path().join("a-moved")).unwrap();
    junction(&tmp.path().join("a"), &other);
    let e = remove_verified(&item, Kind::Dir, dev, ino).unwrap_err();
    assert!(e.to_string().contains("no longer the item"), "{e}");
    assert!(other.join("b/item/precious").exists());
    let file = tmp.path().join("a/b/file");
    let e = remove_verified(&file, Kind::File, fdev, fino).unwrap_err();
    assert!(e.to_string().contains("no longer the item"), "{e}");
    assert!(other.join("b/file").exists());
    // Where they are now, they are deleted.
    let moved = tmp.path().join("a-moved/b");
    remove_verified(&moved.join("item"), Kind::Dir, dev, ino).unwrap();
    remove_verified(&moved.join("file"), Kind::File, fdev, fino).unwrap();
    assert!(!moved.join("item").exists() && !moved.join("file").exists());
}

/// While the item is held for deleting, a directory above it cannot be renamed, so the
/// paths of its contents stay inside it.
#[test]
fn the_held_item_pins_the_directories_above() {
    use std::os::windows::io::AsHandle;
    let tmp = tempfile::tempdir().unwrap();
    let item = tmp.path().join("a/item");
    std::fs::create_dir_all(&item).unwrap();
    let h = dfdu_sys::open_for_delete(&item).unwrap();
    assert!(std::fs::rename(tmp.path().join("a"), tmp.path().join("a2")).is_err());
    assert!(std::fs::rename(&item, tmp.path().join("item2")).is_err());
    dfdu_sys::delete_open(h.as_handle()).unwrap();
    drop(h);
    assert!(!item.exists());
}

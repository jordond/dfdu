use std::os::windows::io::AsHandle;

use super::super::open_dir;
use super::*;

/// One entry of `class`'s layout, `next` bytes before the following one (0: the last).
fn entry(class: Class, next: u32, name: &str, stat: &WinStat) -> Vec<u8> {
    let l = class.layout();
    let units: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut b = vec![0u8; l.name + units.len()];
    b.resize(b.len().next_multiple_of(8), 0);
    let mut put = |at: usize, v: &[u8]| b[at..at + v.len()].copy_from_slice(v);
    put(0, &next.to_le_bytes());
    put(l.last_write, &stat.last_write.to_le_bytes());
    put(l.size, &stat.size.to_le_bytes());
    put(l.alloc, &stat.alloc.to_le_bytes());
    put(l.attributes, &stat.attributes.to_le_bytes());
    put(l.name_len, &len_u32(units.len()).to_le_bytes());
    put(l.tag, &stat.reparse_tag.to_le_bytes());
    match l.id {
        (at, 16) => put(at, &stat.file_id.to_le_bytes()),
        (at, 8) => put(at, &u64::try_from(stat.file_id).unwrap().to_le_bytes()),
        _ => {}
    }
    put(l.name, &units);
    b
}

/// Entries chained in one buffer, as one call of `class` would leave them.
fn chain(class: Class, entries: &[(&str, WinStat)]) -> Vec<u8> {
    let mut out = vec![];
    for (i, (name, stat)) in entries.iter().enumerate() {
        let len = entry(class, 0, name, stat).len();
        let next = if i + 1 == entries.len() { 0 } else { len };
        out.extend(entry(class, len_u32(next), name, stat));
    }
    out
}

/// A reader whose buffer holds `bytes` and that has nothing more to read after them.
fn loaded(class: Class, bytes: &[u8]) -> DirReader {
    let mut r = DirReader {
        class,
        next: Some(0),
        started: true,
        done: true,
        ..DirReader::default()
    };
    for (w, c) in r.buf.iter_mut().zip(bytes.chunks(8)) {
        let mut word = [0u8; 8];
        word[..c.len()].copy_from_slice(c);
        *w = u64::from_le_bytes(word);
    }
    r
}

fn read_all(r: &mut DirReader) -> io::Result<Vec<(String, WinStat)>> {
    // The handle is not used: the reader has its buffer and is done after it.
    let f = tempfile::tempfile()?;
    let mut out = vec![];
    while let Some(e) = r.next(f.as_handle())? {
        let units: Vec<u16> = e.name_units().collect();
        out.push((String::from_utf16(&units).unwrap(), e.stat));
    }
    Ok(out)
}

fn stat(attributes: u32, reparse_tag: u32, file_id: u128) -> WinStat {
    WinStat {
        attributes,
        reparse_tag,
        size: 5000,
        alloc: 8192,
        last_write: 133_000_000_000_000_000,
        volume: 0,
        file_id,
        nlink: 1,
    }
}

/// `.` and `..` are left out, the reparse tag is read only for reparse points, and the file
/// id is as wide as the class gives it.
#[test]
fn entries_are_read_from_each_class_layout() {
    let plain = stat(0x20, 0, 0x0102_0304);
    let link = stat(
        win_attr::DIRECTORY | win_attr::REPARSE_POINT,
        0xA000_000C,
        9,
    );
    // A tag in a non-reparse entry (`EaSize` in the older classes) is not one.
    let not_link = WinStat {
        reparse_tag: 77,
        ..stat(win_attr::DIRECTORY, 0, 10)
    };
    for class in [Class::IdExtd, Class::IdBoth, Class::Full] {
        let b = chain(
            class,
            &[
                (".", not_link),
                ("..", not_link),
                ("a", plain),
                ("link", link),
                ("sub", not_link),
            ],
        );
        let got = read_all(&mut loaded(class, &b)).unwrap();
        let id = |s: WinStat| WinStat {
            file_id: if class == Class::Full { 0 } else { s.file_id },
            ..s
        };
        let want = vec![
            ("a".to_string(), id(plain)),
            ("link".to_string(), id(link)),
            (
                "sub".to_string(),
                WinStat {
                    reparse_tag: 0,
                    ..id(not_link)
                },
            ),
        ];
        assert_eq!(got, want, "{class:?}");
    }
    let wide = stat(0x20, 0, u128::MAX - 5);
    let b = chain(Class::IdExtd, &[("w", wide)]);
    assert_eq!(read_all(&mut loaded(Class::IdExtd, &b)).unwrap()[0].1, wide);
}

#[test]
fn malformed_entries_are_refused() {
    let s = stat(0x20, 0, 1);
    let l = Class::IdExtd.layout();
    let mut odd = chain(Class::IdExtd, &[("a", s)]);
    odd[l.name_len..l.name_len + 4].copy_from_slice(&3u32.to_le_bytes());
    let mut past_name = chain(Class::IdExtd, &[("a", s)]);
    past_name[l.name_len..l.name_len + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    let mut past_next = chain(Class::IdExtd, &[("a", s)]);
    past_next[..4].copy_from_slice(&len_u32(DIR_BUF_WORDS * 8).to_le_bytes());
    for (what, b) in [("odd", odd), ("name", past_name), ("next", past_next)] {
        let e = read_all(&mut loaded(Class::IdExtd, &b)).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData, "{what}");
    }
}

#[test]
fn classes_fall_back_to_fewer_fields() {
    assert_eq!(Class::IdExtd.fallback(), Some(Class::IdBoth));
    assert_eq!(Class::IdBoth.fallback(), Some(Class::Full));
    assert_eq!(Class::Full.fallback(), None);
}

/// A directory larger than one buffer is read whole, each entry once; an empty one has none.
#[test]
fn reads_directories_across_buffers() {
    let tmp = tempfile::tempdir().unwrap();
    let names: Vec<String> = (0..1500)
        .map(|i| format!("a-name-long-enough-to-fill-buffers-{i:04}"))
        .collect();
    for n in &names {
        std::fs::write(tmp.path().join(n), b"x").unwrap();
    }
    std::fs::create_dir(tmp.path().join("empty")).unwrap();
    let dir = open_dir(tmp.path(), false).unwrap();
    let mut r = DirReader::default();
    let mut got = vec![];
    while let Some(e) = r.next(dir.as_handle()).unwrap() {
        got.push(String::from_utf16(&e.name_units().collect::<Vec<_>>()).unwrap());
    }
    got.sort();
    let mut want = names;
    want.push("empty".into());
    assert_eq!(got, want);
    let empty = open_dir(&tmp.path().join("empty"), false).unwrap();
    let mut r = DirReader::default();
    assert!(r.next(empty.as_handle()).unwrap().is_none());
    assert!(
        r.next(empty.as_handle()).unwrap().is_none(),
        "the end stays the end"
    );
}

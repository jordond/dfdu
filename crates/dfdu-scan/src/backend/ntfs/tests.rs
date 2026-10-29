use super::*;

/// Builds file records as NTFS writes them, update sequence included.
pub struct Rec {
    buf: Vec<u8>,
    at: usize,
}

impl Rec {
    pub fn new(seq: u16, flags: u16, base: u64) -> Rec {
        let mut buf = vec![0u8; 1024];
        buf[..4].copy_from_slice(b"FILE");
        buf[4..6].copy_from_slice(&0x30u16.to_le_bytes());
        buf[6..8].copy_from_slice(&3u16.to_le_bytes());
        buf[0x10..0x12].copy_from_slice(&seq.to_le_bytes());
        buf[0x14..0x16].copy_from_slice(&0x38u16.to_le_bytes());
        buf[0x16..0x18].copy_from_slice(&flags.to_le_bytes());
        buf[0x1C..0x20].copy_from_slice(&1024u32.to_le_bytes());
        buf[0x20..0x28].copy_from_slice(&base.to_le_bytes());
        Rec { buf, at: 0x38 }
    }

    fn attr(&mut self, ty: u32, body: &[u8]) -> &mut Self {
        let len = body.len().div_ceil(8) * 8;
        let a = &mut self.buf[self.at..self.at + len];
        a[..body.len()].copy_from_slice(body);
        a[..4].copy_from_slice(&ty.to_le_bytes());
        a[4..8].copy_from_slice(&u32::try_from(len).unwrap().to_le_bytes());
        self.at += len;
        self
    }

    fn resident(&mut self, ty: u32, name_len: u8, value: &[u8]) -> &mut Self {
        let mut body = vec![0u8; 0x18];
        body[9] = name_len;
        body[0x10..0x14].copy_from_slice(&u32::try_from(value.len()).unwrap().to_le_bytes());
        body[0x14..0x16].copy_from_slice(&0x18u16.to_le_bytes());
        body.extend(value);
        self.attr(ty, &body)
    }

    pub fn info(&mut self, attributes: u32, mtime: u64) -> &mut Self {
        let mut v = vec![0u8; 48];
        v[8..16].copy_from_slice(&mtime.to_le_bytes());
        v[32..36].copy_from_slice(&attributes.to_le_bytes());
        self.resident(AT_STANDARD_INFORMATION, 0, &v)
    }

    pub fn name(&mut self, parent: u64, name: &str, ns: u8) -> &mut Self {
        self.name_tagged(parent, name, ns, 0)
    }

    pub fn name_tagged(&mut self, parent: u64, name: &str, ns: u8, tag: u32) -> &mut Self {
        let units: Vec<u16> = name.encode_utf16().collect();
        let mut v = vec![0u8; 0x42];
        v[..8].copy_from_slice(&parent.to_le_bytes());
        if tag != 0 {
            v[0x38..0x3C].copy_from_slice(&win_attr::REPARSE_POINT.to_le_bytes());
            v[0x3C..0x40].copy_from_slice(&tag.to_le_bytes());
        }
        v[0x40] = u8::try_from(units.len()).unwrap();
        v[0x41] = ns;
        v.extend(units.iter().flat_map(|u| u.to_le_bytes()));
        self.resident(AT_FILE_NAME, 0, &v)
    }

    pub fn small(&mut self, data: &[u8]) -> &mut Self {
        self.resident(AT_DATA, 0, data)
    }

    pub fn big(&mut self, ty: u32, flags: u16, alloc: u64, size: u64, total: u64) -> &mut Self {
        let mut body = vec![0u8; 0x48];
        body[8] = 1;
        body[0x0C..0x0E].copy_from_slice(&flags.to_le_bytes());
        body[0x20..0x22].copy_from_slice(&0x48u16.to_le_bytes());
        // Last cluster: the one run below.
        body[0x18..0x20].copy_from_slice(&0u64.to_le_bytes());
        body[0x28..0x30].copy_from_slice(&alloc.to_le_bytes());
        body[0x30..0x38].copy_from_slice(&size.to_le_bytes());
        body[0x40..0x48].copy_from_slice(&total.to_le_bytes());
        // One run: 1 cluster at 100.
        body.extend([0x11, 1, 100, 0, 0, 0, 0, 0]);
        self.attr(ty, &body)
    }

    /// The record as on disk: end marker, used length and update sequence applied.
    pub fn bytes(&mut self) -> Vec<u8> {
        let at = self.at;
        self.buf[at..at + 4].copy_from_slice(&AT_END.to_le_bytes());
        self.buf[0x18..0x1C].copy_from_slice(&u32::try_from(at + 8).unwrap().to_le_bytes());
        let mut b = self.buf.clone();
        let check = [0x5A, 0xA5];
        b[0x30..0x32].copy_from_slice(&check);
        for i in 1..=2 {
            let end = i * STRIDE;
            b[0x30 + 2 * i] = b[end - 2];
            b[0x30 + 2 * i + 1] = b[end - 1];
            b[end - 2..end].copy_from_slice(&check);
        }
        b
    }
}

fn reference(n: u64, seq: u16) -> u64 {
    u64::from(seq) << 48 | n
}

#[test]
fn boot_sectors_are_read() {
    let mut b = vec![0u8; 512];
    b[3..11].copy_from_slice(b"NTFS    ");
    b[0x0B..0x0D].copy_from_slice(&512u16.to_le_bytes());
    b[0x0D] = 8;
    b[0x30..0x38].copy_from_slice(&786_432u64.to_le_bytes());
    b[0x40] = 0xF6; // -10: 1024-byte records
    assert_eq!(
        Boot::parse(&b),
        Some(Boot {
            sector: 512,
            cluster: 4096,
            mft_lcn: 786_432,
            record: 1024
        })
    );
    b[0x0D] = 0xF4; // 2^12 sectors per cluster
    assert_eq!(Boot::parse(&b).map(|b| b.cluster), Some(2 << 20));
    b[0x0D] = 3;
    assert_eq!(Boot::parse(&b), None);
    b[0x0D] = 8;
    b[3] = b'X';
    assert_eq!(Boot::parse(&b), None);
}

#[test]
fn data_runs_are_decoded() {
    // 0x18 clusters at 0x5634; 0x10 sparse; 0x20 at 0x5634 - 0x10.
    let b = [0x21, 0x18, 0x34, 0x56, 0x01, 0x10, 0x11, 0x20, 0xF0, 0x00];
    assert_eq!(
        data_runs(&b),
        Some(vec![
            Run {
                lcn: Some(0x5634),
                len: 0x18
            },
            Run {
                lcn: None,
                len: 0x10
            },
            Run {
                lcn: Some(0x5624),
                len: 0x20
            },
        ])
    );
    assert_eq!(data_runs(&[0x21, 0x18]), None);
    // Below cluster 0.
    assert_eq!(data_runs(&[0x11, 1, 0x80, 0]), None);
}

#[test]
fn fixups_restore_sector_ends_and_catch_torn_writes() {
    let mut r = Rec::new(1, REC_IN_USE, 0);
    r.buf[510] = 0x11;
    r.buf[1023] = 0x22;
    let mut b = r.bytes();
    assert!(fixup(&mut b));
    assert_eq!((b[510], b[1023]), (0x11, 0x22));
    let mut torn = r.bytes();
    torn[1022] ^= 1;
    assert!(!fixup(&mut torn));
}

/// A small tree: the root, a directory, files resident and not, compressed and sparse
/// sizes, a hard link, a DOS alias, a junction, a name in an extension record, a stale name
/// and the filesystem's own files.
pub fn volume() -> MftBuilder {
    let root = reference(ROOT_RECORD, 5);
    let mut b = MftBuilder::default();
    let mut add = |n: u64, r: &mut Rec| b.add(n, &mut r.bytes());
    add(
        0,
        Rec::new(1, REC_IN_USE, 0).info(6, 1).name(root, "$MFT", 3),
    );
    add(
        ROOT_RECORD,
        Rec::new(5, REC_IN_USE | REC_DIRECTORY, 0)
            .info(6, 1)
            .name(root, ".", 3),
    );
    let docs = reference(30, 2);
    add(
        30,
        Rec::new(2, REC_IN_USE | REC_DIRECTORY, 0)
            .info(0x10, 7)
            .name(root, "Documents", 1)
            .name(root, "DOCUME~1", NS_DOS)
            .big(AT_INDEX_ALLOCATION, 0, 4096, 4096, 0),
    );
    add(
        31,
        Rec::new(1, REC_IN_USE, 0)
            .info(0x20, 99)
            .name(docs, "note.txt", 3)
            .small(b"hello"),
    );
    add(
        32,
        Rec::new(9, REC_IN_USE, 0)
            .info(0x20, 98)
            .name(docs, "big.bin", 1)
            .name(root, "link.bin", 1)
            .big(AT_DATA, 0, 8192, 5000, 0),
    );
    add(
        33,
        Rec::new(1, REC_IN_USE, 0)
            .info(0x820, 97)
            .name(docs, "packed", 1)
            .big(AT_DATA, ATTR_COMPRESSED, 65536, 60000, 12288),
    );
    add(
        34,
        Rec::new(1, REC_IN_USE | REC_DIRECTORY, 0)
            .info(0x410, 96)
            .name_tagged(root, "junction", 1, 0xA000_0003),
    );
    // A second name in an extension record of 32.
    add(
        35,
        Rec::new(1, REC_IN_USE, reference(32, 9)).name(docs, "alias.bin", 1),
    );
    // A name in a directory record since reused (sequence 1, the directory is at 2).
    add(
        36,
        Rec::new(1, REC_IN_USE, 0)
            .info(0x20, 1)
            .name(reference(30, 1), "stale", 1)
            .small(b""),
    );
    // Deleted.
    add(37, Rec::new(1, 0, 0).info(0x20, 1).name(docs, "gone", 1));
    b
}

fn names(ix: &MftIndex, dir: u64) -> Vec<(String, WinStat)> {
    let mut v: Vec<_> = ix
        .children(dir)
        .unwrap()
        .iter()
        .map(|e| (String::from_utf16_lossy(ix.name(e)), e.stat))
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

#[test]
fn records_become_listings() {
    let b = volume();
    assert_eq!(b.damaged, 0);
    let ix = b.finish();
    let root = names(&ix, reference(ROOT_RECORD, 5));
    let root_names: Vec<&str> = root.iter().map(|e| e.0.as_str()).collect();
    assert_eq!(root_names, ["Documents", "junction", "link.bin"]);
    let docs = &root[0].1;
    assert_eq!(docs.attributes & win_attr::DIRECTORY, win_attr::DIRECTORY);
    assert_eq!((docs.size, docs.alloc, docs.nlink), (0, 4096, 1));
    assert_eq!(docs.file_id, u128::from(reference(30, 2)));
    let j = &root[1].1;
    assert_eq!(j.reparse_tag, 0xA000_0003);
    assert_eq!(j.kind(), dfdu_tree::Kind::Symlink);

    let d = names(&ix, reference(30, 2));
    let dn: Vec<&str> = d.iter().map(|e| e.0.as_str()).collect();
    assert_eq!(dn, ["alias.bin", "big.bin", "note.txt", "packed"]);
    let big = &d[1].1;
    assert_eq!(
        (big.size, big.alloc, big.nlink, big.last_write),
        (5000, 8192, 3, 98)
    );
    assert_eq!(d[0].1, *big);
    assert_eq!(root[2].1, *big);
    assert_eq!((d[2].1.size, d[2].1.alloc), (5, 8));
    assert_eq!((d[3].1.size, d[3].1.alloc), (60000, 12288));
    // Another sequence number is another directory.
    assert!(ix.children(reference(30, 1)).is_none());
    assert!(ix.children(reference(31, 1)).is_none());
}

#[test]
fn the_mft_stream_is_found() {
    let mut r = Rec::new(1, REC_IN_USE, 0);
    r.info(6, 1)
        .name(reference(ROOT_RECORD, 5), "$MFT", 3)
        .big(AT_DATA, 0, 4096, 4096, 0);
    let mut b = r.bytes();
    assert!(fixup(&mut b));
    assert_eq!(
        mft_data(&b),
        Some((
            vec![Run {
                lcn: Some(100),
                len: 1
            }],
            4096
        ))
    );
    let mut r = Rec::new(1, REC_IN_USE, 0);
    r.small(b"resident");
    let mut b = r.bytes();
    assert!(fixup(&mut b));
    assert_eq!(mft_data(&b), None);
}

/// No cut, bit flip or overwrite of a record panics.
#[test]
fn damaged_records_never_panic() {
    let mut r = Rec::new(3, REC_IN_USE, 0);
    r.info(0x20, 5)
        .name(reference(ROOT_RECORD, 5), "x", 1)
        .big(AT_DATA, ATTR_SPARSE, 4096, 100, 0);
    let good = r.bytes();
    let mut b = MftBuilder::default();
    for i in 0..good.len() * 8 {
        let mut rec = good.clone();
        rec[i / 8] ^= 1 << (i % 8);
        b.add(40, &mut rec);
        if fixup(&mut rec.clone()) {
            let _ = mft_data(&rec);
        }
    }
    for i in 0..good.len() {
        let mut rec = good.clone();
        rec[i] = 0xFF;
        b.add(41, &mut rec);
        let mut cut = good[..i - i % STRIDE].to_vec();
        b.add(42, &mut cut);
    }
    let _ = b.finish();
}

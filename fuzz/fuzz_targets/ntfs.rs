//! The MFT backend's parser: the input as a boot sector, a mapping pairs array, and a run of
//! 1024-byte file records, both as they are and with a valid header and update sequence (so
//! the attributes are reached), then listed.
#![no_main]

use dfdu_scan::ntfs::{Boot, MftBuilder, data_runs, fixup, mft_data};
use libfuzzer_sys::fuzz_target;

const RECORD: usize = 1024;

fuzz_target!(|data: &[u8]| {
    let _ = Boot::parse(data);
    let _ = data_runs(data);
    let mut raw = MftBuilder::default();
    let mut fixed = MftBuilder::default();
    for (n, chunk) in data.chunks(RECORD).enumerate() {
        let mut rec = chunk.to_vec();
        rec.resize(RECORD, 0);
        raw.add(n as u64, &mut rec.clone());
        rec[..4].copy_from_slice(b"FILE");
        rec[4..6].copy_from_slice(&0x30u16.to_le_bytes());
        rec[6..8].copy_from_slice(&3u16.to_le_bytes());
        for i in 1..=2 {
            let end = i * 512;
            rec[end - 2] = rec[0x30];
            rec[end - 1] = rec[0x31];
        }
        let mut probe = rec.clone();
        if fixup(&mut probe) {
            let _ = mft_data(&probe);
        }
        fixed.add(n as u64, &mut rec);
    }
    for b in [raw, fixed] {
        let ix = b.finish();
        let dirs: Vec<u64> = ix.dirs().collect();
        for d in dirs {
            for e in ix.children(d).unwrap_or(&[]) {
                let _ = ix.name(e);
            }
        }
    }
});

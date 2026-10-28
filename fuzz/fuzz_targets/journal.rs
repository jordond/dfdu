//! The deletion journal as recovery reads it: one journal of an exited session in an otherwise
//! empty journal directory. Recovery only reads here; nothing is finished or restored.
#![no_main]

use std::sync::OnceLock;

use libfuzzer_sys::fuzz_target;

fn dir() -> &'static tempfile::TempDir {
    static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    DIR.get_or_init(|| tempfile::tempdir().expect("temp dir"))
}

fuzz_target!(|data: &[u8]| {
    for line in String::from_utf8_lossy(data).lines() {
        let _ = dfdu_clean::check_line(line);
    }
    let ops = dir().path().join("ops");
    std::fs::create_dir_all(&ops).expect("ops");
    std::fs::write(ops.join("1-1-1.jsonl"), data).expect("journal");
    if let Ok(r) = dfdu_clean::recover::scan(&ops) {
        for u in &r.items {
            let _ = u.describe();
        }
    }
});

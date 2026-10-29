use super::*;
use std::os::unix::fs::MetadataExt;
use std::sync::mpsc;

/// Mounts, or prints why not and returns `None` (no `/dev/fuse`, no privilege).
fn mount(spec: Spec, at: &Path, opts: Options) -> Option<Mounted> {
    match Mounted::mount(spec, at, opts) {
        Ok(m) => Some(m),
        Err(e) => {
            eprintln!("skipped: cannot mount a FUSE filesystem: {e}");
            None
        }
    }
}

#[test]
fn serves_the_tree_and_hangs_selected_nodes() {
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path().to_path_buf();
    let mut spec = Spec::new();
    let a = spec.dir(0, "a");
    spec.file(a, "f", 5000);
    let hung = spec.dir(0, "hung");
    let stuck = spec.file(0, "stuck", 1);
    spec.hang(hung);
    spec.hang(stuck);
    let opts = Options {
        subtype: Some("sshfs".into()),
        ..Default::default()
    };
    let Some(mounted) = mount(spec, &tmp, opts) else {
        return;
    };
    let names: Vec<String> = std::fs::read_dir(&tmp)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, ["a", "hung", "stuck"]);
    let meta = std::fs::metadata(tmp.join("a/f")).unwrap();
    assert_eq!((meta.len(), meta.blocks()), (5000, 16));
    let table = std::fs::read_to_string("/proc/self/mountinfo").unwrap();
    assert!(table.contains("fuse.sshfs"), "{table}");
    // A listing and a stat that never answer, until released.
    let (tx, rx) = mpsc::channel();
    for p in ["hung", "stuck"] {
        let (tx, path) = (tx.clone(), tmp.join(p));
        thread::spawn(move || {
            let r = if p == "hung" {
                std::fs::read_dir(&path)
                    .and_then(|d| d.collect::<Result<Vec<_>, _>>().map(|v| v.len()))
            } else {
                std::fs::metadata(&path).map(|_| 0)
            };
            tx.send(r.is_err()).unwrap();
        });
    }
    assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
    assert_eq!(mounted.held(), 2);
    mounted.release();
    for _ in 0..2 {
        assert!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), "EIO");
    }
    drop(mounted);
    // Unmounted: the directory is empty again.
    assert_eq!(std::fs::read_dir(&tmp).unwrap().count(), 0);
}

use super::*;

pub(super) fn map(entries: &[(&str, u64)]) -> SizeMap {
    entries
        .iter()
        .map(|&(p, d)| {
            (
                p.as_bytes().to_vec(),
                Sizes {
                    disk: d,
                    apparent: d / 2,
                    entries: 3,
                },
            )
        })
        .collect()
}

/// Keys already in a database keep matching: the format must not change.
#[test]
fn options_key_names_the_mount_options_and_measuring() {
    let job = JobOptions {
        network: true,
        ..JobOptions::default()
    };
    assert_eq!(
        options_key(&job, false),
        "cross_mounts=false network=true measured=false"
    );
}

/// Connections that open a new database and record at the same time all succeed: each
/// waits for the others instead of failing with `SQLITE_BUSY`.
#[test]
fn concurrent_opens_and_records_wait_for_each_other() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("history.sqlite3");
    let rows = map(&[("", 10)]);
    std::thread::scope(|s| {
        for i in 0..8 {
            let (db, rows) = (&db, &rows);
            s.spawn(move || {
                for j in 0..5 {
                    let mut h = History::open(db).unwrap();
                    record(&mut h, "/r", "k", 100 + i * 10 + j, rows);
                }
            });
        }
    });
    assert_eq!(History::open(&db).unwrap().scans(None).unwrap().len(), 40);
}

/// Opening a database already at the schema only reads it, so it does not queue behind a
/// connection holding the write lock; commits sync only at checkpoints.
#[test]
fn opening_a_current_database_does_not_wait_for_a_writer() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("history.sqlite3");
    let mut writer = History::open(&db).unwrap();
    let tx = writer.write().unwrap();
    let t0 = std::time::Instant::now();
    let h = History::open(&db).unwrap();
    assert!(t0.elapsed() < BUSY / 2, "{:?}", t0.elapsed());
    let sync: i64 = (h.conn)
        .query_row("PRAGMA synchronous", [], |r| r.get(0))
        .unwrap();
    assert_eq!(sync, 1, "NORMAL");
    drop(tx);
}

fn record(h: &mut History, root: &str, options: &str, at: i64, rows: &SizeMap) -> i64 {
    h.record(&Record {
        root: Path::new(root),
        options,
        started_at: at - 5,
        finished_at: at,
        complete: true,
        total: rows.get(&b""[..]).copied().unwrap_or_default(),
        errors: 0,
        rows,
    })
    .unwrap()
}

#[test]
fn scans_are_recorded_and_found() {
    let mut h = History::in_memory().unwrap();
    let old = map(&[("", 100), ("a", 60), ("a/b", 10)]);
    let new = map(&[("", 300), ("a", 260), ("c", 40)]);
    let a = record(&mut h, "/r", "x", 1_000, &old);
    let b = record(&mut h, "/r", "x", 2_000, &new);
    let other = record(&mut h, "/r", "y", 2_500, &new);
    record(&mut h, "/s", "x", 3_000, &new);
    assert_eq!(h.sizes(a).unwrap(), old);
    assert_eq!(h.sizes(b).unwrap(), new);
    let r = Path::new("/r");
    assert_eq!(h.latest(r, "x", None).unwrap().unwrap().id, b);
    assert_eq!(h.latest(r, "x", Some(2_000)).unwrap().unwrap().id, a);
    assert_eq!(h.latest(r, "x", Some(1_000)).unwrap(), None);
    assert_eq!(h.latest(r, "y", None).unwrap().unwrap().id, other);
    assert_eq!(h.nearest(r, "x", 1_400).unwrap().unwrap().id, a);
    assert_eq!(h.nearest(r, "x", 1_600).unwrap().unwrap().id, b);
    let all = h.scans(Some(r)).unwrap();
    assert_eq!(all.iter().map(|s| s.id).collect::<Vec<_>>(), [other, b, a]);
    assert_eq!(all[1].total.disk, 300);
    assert_eq!(h.scans(None).unwrap().len(), 4);
}

#[test]
fn growth_finds_sizes_by_path() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("history.sqlite3");
    assert!(
        Growth::load(&db, Path::new("/r"), "x", None)
            .unwrap()
            .is_none()
    );
    let mut h = History::open(&db).unwrap();
    let sep = std::path::MAIN_SEPARATOR.to_string();
    let rows = map(&[("", 9), ("a", 7), (&format!("a{sep}b"), 3)]);
    record(&mut h, "/r", "x", 86_400 * 3, &rows);
    let g = Growth::load(&db, Path::new("/r"), "x", None)
        .unwrap()
        .unwrap();
    assert_eq!(g.at, 86_400 * 3);
    assert_eq!(g.get(Path::new("/r")).unwrap().disk, 9);
    assert_eq!(g.get(&Path::new("/r").join("a").join("b")).unwrap().disk, 3);
    assert_eq!(g.get(Path::new("/r/c")), None);
    assert_eq!(g.get(Path::new("/s")), None);
    assert!(
        Growth::load(&db, Path::new("/r"), "y", None)
            .unwrap()
            .is_none()
    );
    assert_eq!(day(86_400 * 3), "1970-01-04");
    assert_eq!(day(1_793_157_288), "2026-10-28");
}

#[test]
fn dates_and_times_are_utc() {
    assert_eq!(date_time(0), "1970-01-01 00:00 UTC");
    assert_eq!(date_time(1_793_157_288), "2026-10-28 03:14 UTC");
    assert_eq!(date_time(951_782_400), "2000-02-29 00:00 UTC");
    assert_eq!(date_time(-86_400), "1969-12-31 00:00 UTC");
    assert_eq!(date_time(-1), "1969-12-31 23:59 UTC");
    assert_eq!(day(-1), "1969-12-31");
}

#[test]
fn old_scans_are_dropped() {
    let mut h = History::in_memory().unwrap();
    let rows = map(&[("", 1), ("a", 1)]);
    let first = record(&mut h, "/r", "x", 0, &rows);
    for i in 1..=KEEP_SCANS {
        record(&mut h, "/r", "x", i, &rows);
    }
    assert_eq!(
        h.scans(None).unwrap().len(),
        usize::try_from(KEEP_SCANS).unwrap()
    );
    assert!(h.sizes(first).is_err(), "its scan is gone");
    let n: i64 = h
        .conn
        .query_row("SELECT count(*) FROM dir_sizes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 2 * KEEP_SCANS, "its sizes went with it");
}

#[test]
fn a_newer_schema_is_refused_and_an_existing_one_reopened() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("h/history.sqlite3");
    let mut h = History::open(&p).unwrap();
    let id = record(&mut h, "/r", "x", 10, &map(&[("", 5)]));
    drop(h);
    let h = History::open(&p).unwrap();
    assert_eq!(h.sizes(id).unwrap(), map(&[("", 5)]));
    h.conn.pragma_update(None, "user_version", 7).unwrap();
    drop(h);
    let e = History::open(&p).err().unwrap();
    assert!(e.to_string().contains("newer dfdu"), "{e}");
}

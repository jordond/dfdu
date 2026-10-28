//! Scan history (`safety-persistence-ops.md §3.4`): a SQLite database in the state directory
//! with one summary row per scan and the sizes of a few thousand directories of it, the top
//! levels, the big ones and every match of the rules. It is written after a scan finishes,
//! never during it, in one transaction. Growth is only measured between scans of the same root
//! with the same options, since other mount options count other data.
//!
//! The database runs in WAL mode with a busy timeout, so a browser and a `dfdu scan` from
//! cron can both write. `PRAGMA user_version` is the schema version; [`History::open`] brings an
//! older database up to date and refuses a newer one.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use dfdu_tree::{Ix, Tree};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

/// The schema this version writes.
const SCHEMA: i64 = 1;
/// Directories this many levels below the root or fewer are kept.
pub const KEEP_DEPTH: usize = 4;
/// Directories at least this big are kept at any depth.
pub const KEEP_SIZE: u64 = 100 << 20;
/// Most directories kept per scan; the smallest go first.
pub const MAX_ROWS: usize = 100_000;
/// Scans kept per root and options; older ones are dropped.
pub const KEEP_SCANS: i64 = 500;
const BUSY: Duration = Duration::from_secs(5);

/// Disk usage, apparent size and entries of one directory, or of a whole scan.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Sizes {
    pub disk: u64,
    pub apparent: u64,
    pub entries: u64,
}

/// Paths relative to the scan root (names joined by `/`, the root itself empty) and their
/// sizes.
pub type SizeMap = BTreeMap<Vec<u8>, Sizes>;

/// One recorded scan.
#[derive(Clone, Debug, PartialEq)]
pub struct ScanRow {
    pub id: i64,
    pub root: PathBuf,
    /// What the scan counted (mount options), as [`Record::options`].
    pub options: String,
    pub started_at: i64,
    pub finished_at: i64,
    pub complete: bool,
    pub total: Sizes,
    pub errors: u64,
}

/// A finished scan to record.
pub struct Record<'a> {
    pub root: &'a Path,
    /// A key for the scan's options: only scans with the same key are compared.
    pub options: &'a str,
    pub started_at: i64,
    pub finished_at: i64,
    pub complete: bool,
    pub total: Sizes,
    pub errors: u64,
    pub rows: &'a SizeMap,
}

pub struct History {
    conn: Connection,
}

fn err(e: rusqlite::Error) -> io::Error {
    io::Error::other(e)
}

fn path_bytes(p: &Path) -> &[u8] {
    p.as_os_str().as_encoded_bytes()
}

fn path_of(b: Vec<u8>) -> PathBuf {
    // The bytes came from `as_encoded_bytes` on this platform, or from another dfdu writing
    // the same database; anything else is shown lossily.
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        PathBuf::from(std::ffi::OsString::from_vec(b))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(&b).into_owned())
    }
}

/// The key under which scans are compared: scans that count other data (other mounts, shared
/// extents measured or not) are never compared with each other.
pub fn options_key(cross_mounts: bool, network: bool, measured: bool) -> String {
    format!("cross_mounts={cross_mounts} network={network} measured={measured}")
}

/// Seconds since the Unix epoch as a UTC date ("2026-10-18").
pub fn day(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    // Days to a civil date, in 400-year eras starting on 1 March.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + (m <= 2) as i64;
    format!("{y:04}-{m:02}-{d:02}")
}

/// An earlier scan's directory sizes, to show what grew since.
#[derive(Clone, Debug)]
pub struct Growth {
    pub root: PathBuf,
    /// When that scan finished.
    pub at: i64,
    pub sizes: SizeMap,
}

impl Growth {
    /// The sizes `path` had then, if they were kept.
    pub fn get(&self, path: &Path) -> Option<Sizes> {
        let rel = path.strip_prefix(&self.root).ok()?;
        self.sizes.get(rel.as_os_str().as_encoded_bytes()).copied()
    }

    /// The last scan of `root` with `options` that finished before `before`, in the database
    /// at `db`, if there is one.
    pub fn load(
        db: &Path,
        root: &Path,
        options: &str,
        before: Option<i64>,
    ) -> io::Result<Option<Growth>> {
        if !db.exists() {
            return Ok(None);
        }
        let h = History::open(db)?;
        let Some(scan) = h.latest(root, options, before)? else {
            return Ok(None);
        };
        Ok(Some(Growth {
            root: root.to_path_buf(),
            at: scan.finished_at,
            sizes: h.sizes(scan.id)?,
        }))
    }
}

/// `$DFDU_STATE_DIR/history.sqlite3`, else `history.sqlite3` in dfdu's directory in the
/// platform state directory (the data directory on Windows).
pub fn default_path() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("DFDU_STATE_DIR") {
        return Some(PathBuf::from(d).join("history.sqlite3"));
    }
    use etcetera::BaseStrategy;
    let dirs = etcetera::choose_base_strategy().ok()?;
    let base = dirs.state_dir().unwrap_or_else(|| dirs.data_dir());
    Some(base.join("dfdu").join("history.sqlite3"))
}

impl History {
    /// Opens or creates the database at `path` (and its directory).
    pub fn open(path: &Path) -> io::Result<History> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path).map_err(err)?;
        Self::setup(conn)
    }

    /// A database in memory, for tests.
    pub fn in_memory() -> io::Result<History> {
        Self::setup(Connection::open_in_memory().map_err(err)?)
    }

    fn setup(conn: Connection) -> io::Result<History> {
        conn.busy_timeout(BUSY).map_err(err)?;
        // Switching a new database to WAL needs it alone; two connections switching at once
        // can get SQLITE_BUSY without the busy handler waiting (each holds what the other
        // needs), so the switch is retried.
        let t0 = std::time::Instant::now();
        loop {
            match conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get::<_, String>(0)) {
                Ok(_) => break,
                Err(rusqlite::Error::SqliteFailure(e, _))
                    if e.code == rusqlite::ErrorCode::DatabaseBusy && t0.elapsed() < BUSY =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(e) => return Err(err(e)),
            }
        }
        conn.pragma_update(None, "foreign_keys", true)
            .map_err(err)?;
        let mut h = History { conn };
        h.migrate()?;
        Ok(h)
    }

    /// A transaction that takes the write lock at its start. A deferred one that reads first
    /// fails at once with `SQLITE_BUSY` when another connection writes in between, without
    /// waiting out the busy timeout.
    fn write(&mut self) -> io::Result<rusqlite::Transaction<'_>> {
        self.conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(err)
    }

    fn migrate(&mut self) -> io::Result<()> {
        let tx = self.write()?;
        let version: i64 = tx
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(err)?;
        if version > SCHEMA {
            return Err(io::Error::other(format!(
                "the history database has schema {version}, written by a newer dfdu (this one \
                 reads {SCHEMA})"
            )));
        }
        if version < 1 {
            tx.execute_batch(
                "CREATE TABLE scans (
                    id INTEGER PRIMARY KEY,
                    root BLOB NOT NULL,
                    options TEXT NOT NULL,
                    started_at INTEGER NOT NULL,
                    finished_at INTEGER NOT NULL,
                    complete INTEGER NOT NULL,
                    disk INTEGER NOT NULL,
                    apparent INTEGER NOT NULL,
                    entries INTEGER NOT NULL,
                    errors INTEGER NOT NULL
                );
                CREATE INDEX scans_by_root ON scans (root, options, finished_at);
                CREATE TABLE paths (
                    id INTEGER PRIMARY KEY,
                    path BLOB NOT NULL UNIQUE
                );
                CREATE TABLE dir_sizes (
                    scan_id INTEGER NOT NULL REFERENCES scans (id) ON DELETE CASCADE,
                    path_id INTEGER NOT NULL REFERENCES paths (id),
                    disk INTEGER NOT NULL,
                    apparent INTEGER NOT NULL,
                    entries INTEGER NOT NULL,
                    PRIMARY KEY (scan_id, path_id)
                ) WITHOUT ROWID;",
            )
            .map_err(err)?;
        }
        tx.pragma_update(None, "user_version", SCHEMA)
            .map_err(err)?;
        tx.commit().map_err(err)
    }

    /// Records a scan in one transaction and drops the oldest scans of its root past
    /// [`KEEP_SCANS`]. Returns its id.
    pub fn record(&mut self, r: &Record) -> io::Result<i64> {
        let tx = self.write()?;
        tx.execute(
            "INSERT INTO scans (root, options, started_at, finished_at, complete, disk, apparent,
                entries, errors) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                path_bytes(r.root),
                r.options,
                r.started_at,
                r.finished_at,
                r.complete,
                r.total.disk as i64,
                r.total.apparent as i64,
                r.total.entries as i64,
                r.errors as i64,
            ],
        )
        .map_err(err)?;
        let id = tx.last_insert_rowid();
        {
            let mut path = tx
                .prepare_cached(
                    "INSERT INTO paths (path) VALUES (?1)
                     ON CONFLICT (path) DO UPDATE SET path = path RETURNING id",
                )
                .map_err(err)?;
            let mut size = tx
                .prepare_cached(
                    "INSERT INTO dir_sizes (scan_id, path_id, disk, apparent, entries)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                )
                .map_err(err)?;
            let mut full = path_bytes(r.root).to_vec();
            let base = full.len();
            for (rel, s) in r.rows {
                full.truncate(base);
                if !rel.is_empty() {
                    full.push(std::path::MAIN_SEPARATOR as u8);
                    full.extend_from_slice(rel);
                }
                let pid: i64 = path.query_row([&full], |row| row.get(0)).map_err(err)?;
                size.execute(params![
                    id,
                    pid,
                    s.disk as i64,
                    s.apparent as i64,
                    s.entries as i64
                ])
                .map_err(err)?;
            }
        }
        tx.execute(
            "DELETE FROM scans WHERE root = ?1 AND options = ?2 AND id NOT IN
                (SELECT id FROM scans WHERE root = ?1 AND options = ?2
                 ORDER BY finished_at DESC, id DESC LIMIT ?3)",
            params![path_bytes(r.root), r.options, KEEP_SCANS],
        )
        .map_err(err)?;
        tx.commit().map_err(err)?;
        Ok(id)
    }

    fn row(r: &rusqlite::Row) -> rusqlite::Result<ScanRow> {
        Ok(ScanRow {
            id: r.get(0)?,
            root: path_of(r.get(1)?),
            options: r.get(2)?,
            started_at: r.get(3)?,
            finished_at: r.get(4)?,
            complete: r.get(5)?,
            total: Sizes {
                disk: r.get::<_, i64>(6)? as u64,
                apparent: r.get::<_, i64>(7)? as u64,
                entries: r.get::<_, i64>(8)? as u64,
            },
            errors: r.get::<_, i64>(9)? as u64,
        })
    }

    const COLUMNS: &str =
        "id, root, options, started_at, finished_at, complete, disk, apparent, entries, errors";

    /// The scans of `root` (every root when `None`), newest first.
    pub fn scans(&self, root: Option<&Path>) -> io::Result<Vec<ScanRow>> {
        let sql = format!(
            "SELECT {} FROM scans WHERE ?1 IS NULL OR root = ?1
             ORDER BY finished_at DESC, id DESC",
            Self::COLUMNS
        );
        let mut st = self.conn.prepare(&sql).map_err(err)?;
        let rows = st
            .query_map([root.map(path_bytes)], Self::row)
            .map_err(err)?;
        rows.collect::<rusqlite::Result<_>>().map_err(err)
    }

    /// The newest scan of `root` with `options` that finished before `before` (any time when
    /// `None`).
    pub fn latest(
        &self,
        root: &Path,
        options: &str,
        before: Option<i64>,
    ) -> io::Result<Option<ScanRow>> {
        let sql = format!(
            "SELECT {} FROM scans WHERE root = ?1 AND options = ?2 AND finished_at < ?3
             ORDER BY finished_at DESC, id DESC LIMIT 1",
            Self::COLUMNS
        );
        self.conn
            .query_row(
                &sql,
                params![path_bytes(root), options, before.unwrap_or(i64::MAX)],
                Self::row,
            )
            .optional()
            .map_err(err)
    }

    /// The scan of `root` with `options` that finished closest to `at`.
    pub fn nearest(&self, root: &Path, options: &str, at: i64) -> io::Result<Option<ScanRow>> {
        let sql = format!(
            "SELECT {} FROM scans WHERE root = ?1 AND options = ?2
             ORDER BY abs(finished_at - ?3), finished_at DESC LIMIT 1",
            Self::COLUMNS
        );
        self.conn
            .query_row(&sql, params![path_bytes(root), options, at], Self::row)
            .optional()
            .map_err(err)
    }

    /// The directory sizes kept for scan `id`, by path relative to its root.
    pub fn sizes(&self, id: i64) -> io::Result<SizeMap> {
        let root: Vec<u8> = self
            .conn
            .query_row("SELECT root FROM scans WHERE id = ?1", [id], |r| r.get(0))
            .map_err(err)?;
        let mut st = self
            .conn
            .prepare(
                "SELECT p.path, d.disk, d.apparent, d.entries FROM dir_sizes d
                 JOIN paths p ON p.id = d.path_id WHERE d.scan_id = ?1",
            )
            .map_err(err)?;
        let rows = st
            .query_map([id], |r| {
                Ok((
                    r.get::<_, Vec<u8>>(0)?,
                    Sizes {
                        disk: r.get::<_, i64>(1)? as u64,
                        apparent: r.get::<_, i64>(2)? as u64,
                        entries: r.get::<_, i64>(3)? as u64,
                    },
                ))
            })
            .map_err(err)?;
        let mut out = SizeMap::new();
        for row in rows {
            let (full, s) = row.map_err(err)?;
            let rel = full
                .strip_prefix(root.as_slice())
                .map(|r| {
                    r.strip_prefix(&[std::path::MAIN_SEPARATOR as u8])
                        .unwrap_or(r)
                })
                .unwrap_or(&full);
            out.insert(rel.to_vec(), s);
        }
        Ok(out)
    }
}

/// The directories of the scan below `root` that history keeps: those at most
/// [`KEEP_DEPTH`] levels down, those of at least [`KEEP_SIZE`], and every match of the rules
/// (files included), at most [`MAX_ROWS`], largest first.
pub fn rows(tree: &Tree, root: Ix) -> SizeMap {
    rows_to(tree, root, KEEP_DEPTH, KEEP_SIZE, MAX_ROWS)
}

/// [`rows`] with other limits: directories at most `depth` levels down or at least `size`
/// bytes, at most `max` of them.
pub fn rows_to(tree: &Tree, root: Ix, depth: usize, size: u64, max: usize) -> SizeMap {
    let sizes = |ix: Ix| {
        let t = tree.total(ix);
        Sizes {
            disk: t.alloc,
            apparent: t.apparent,
            entries: t.items,
        }
    };
    let mut picked: Vec<(Ix, Sizes)> = vec![];
    let mut stack = vec![(root, 0usize)];
    while let Some((ix, d)) = stack.pop() {
        if tree.dir(ix).is_none() {
            continue;
        }
        let s = sizes(ix);
        if d > depth && s.disk < size {
            continue;
        }
        picked.push((ix, s));
        stack.extend(tree.children(ix).map(|c| (c, d + 1)));
    }
    // Matches deeper than the walk went, and matched files.
    for (ix, _) in tree.classes() {
        if tree.is_visible(ix) && is_below(tree, ix, root) {
            picked.push((ix, sizes(ix)));
        }
    }
    picked.sort_by_key(|&(ix, s)| (std::cmp::Reverse(s.disk), ix));
    picked.dedup_by_key(|p| p.0);
    let mut out = SizeMap::new();
    for (ix, s) in picked {
        if out.len() >= max {
            break;
        }
        out.entry(rel_path(tree, ix, root)).or_insert(s);
    }
    out
}

/// How one path changed between two scans. A side is `None` where the path was not there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub path: Vec<u8>,
    pub old: Option<Sizes>,
    pub new: Option<Sizes>,
}

impl Change {
    /// The change in disk usage.
    pub fn delta(&self) -> i128 {
        self.new.map_or(0, |s| s.disk as i128) - self.old.map_or(0, |s| s.disk as i128)
    }

    pub fn apparent_delta(&self) -> i128 {
        self.new.map_or(0, |s| s.apparent as i128) - self.old.map_or(0, |s| s.apparent as i128)
    }
}

/// Levels below the root of a relative path (the root is 0).
pub fn depth(rel: &[u8]) -> usize {
    if rel.is_empty() {
        0
    } else {
        1 + rel
            .iter()
            .filter(|&&b| b == std::path::MAIN_SEPARATOR as u8)
            .count()
    }
}

/// Every path whose sizes differ between `old` and `new`, the biggest change in disk usage
/// first. A path in only one of them is new or gone if it is at most `complete` levels down,
/// where each side has every directory; deeper, it was only left out for being small, so it
/// is not compared (its parents show the change).
pub fn diff(old: &SizeMap, new: &SizeMap, complete: usize) -> Vec<Change> {
    let mut out = vec![];
    for (path, &o) in old {
        match new.get(path) {
            Some(&n) if n == o => {}
            Some(&n) => out.push(Change {
                path: path.clone(),
                old: Some(o),
                new: Some(n),
            }),
            None if depth(path) <= complete => out.push(Change {
                path: path.clone(),
                old: Some(o),
                new: None,
            }),
            None => {}
        }
    }
    for (path, &n) in new {
        if !old.contains_key(path) && depth(path) <= complete {
            out.push(Change {
                path: path.clone(),
                old: None,
                new: Some(n),
            });
        }
    }
    out.sort_by(|a, b| {
        b.delta()
            .abs()
            .cmp(&a.delta().abs())
            .then_with(|| a.path.cmp(&b.path))
    });
    out
}

fn is_below(tree: &Tree, mut ix: Ix, root: Ix) -> bool {
    loop {
        if ix == root {
            return true;
        }
        match tree.parent(ix) {
            Some(p) => ix = p,
            None => return false,
        }
    }
}

/// The names from below `root` down to `ix`, joined by the platform's separator.
pub fn rel_path(tree: &Tree, mut ix: Ix, root: Ix) -> Vec<u8> {
    let mut names = vec![];
    while ix != root {
        names.push(tree.name(ix));
        match tree.parent(ix) {
            Some(p) => ix = p,
            None => break,
        }
    }
    names.reverse();
    names.join(&(std::path::MAIN_SEPARATOR as u8))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(entries: &[(&str, u64)]) -> SizeMap {
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
    fn diff_lists_changes_biggest_first() {
        let sep = std::path::MAIN_SEPARATOR;
        let p = |s: &str| s.replace('/', &sep.to_string());
        let old = map(&[("", 100), (&p("a"), 60), (&p("a/b"), 10), (&p("gone"), 5)]);
        let mut new = map(&[("", 300), (&p("a"), 60), (&p("c"), 40)]);
        new.insert(p("a/b/c/d/e/deep").into_bytes(), Sizes::default());
        let d = diff(&old, &new, 4);
        let got: Vec<(String, i128)> = d
            .iter()
            .map(|c| (String::from_utf8(c.path.clone()).unwrap(), c.delta()))
            .collect();
        assert_eq!(
            got,
            [
                (String::new(), 200),
                ("c".into(), 40),
                (p("a/b"), -10),
                ("gone".into(), -5)
            ]
        );
        assert_eq!(d[1].old, None);
        assert_eq!(d[2].new, None);
        assert_eq!(depth(p("a/b/c").as_bytes()), 3);
        assert_eq!(depth(b""), 0);
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
    fn old_scans_are_dropped() {
        let mut h = History::in_memory().unwrap();
        let rows = map(&[("", 1), ("a", 1)]);
        let first = record(&mut h, "/r", "x", 0, &rows);
        for i in 1..=KEEP_SCANS {
            record(&mut h, "/r", "x", i, &rows);
        }
        assert_eq!(h.scans(None).unwrap().len(), KEEP_SCANS as usize);
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
}

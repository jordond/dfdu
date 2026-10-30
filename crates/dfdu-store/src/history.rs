//! Scan history: a SQLite database in the state directory
//! with one summary row per scan and the sizes of a few thousand directories of it, the top
//! levels, the big ones and every match of the rules. It is written after a scan finishes,
//! never during it, in one transaction. Growth is only measured between scans of the same root
//! with the same options, since other mount options count other data.
//!
//! The database runs in WAL mode with a busy timeout, so a browser and a `dfdu scan` from
//! cron can both write, and syncs at checkpoints rather than every commit, so each holds the
//! write lock only briefly. `PRAGMA user_version` is the schema version; [`History::open`] brings an
//! older database up to date and refuses a newer one.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use dfdu_scan::JobOptions;
use dfdu_tree::{Ix, Totals, Tree, civil_from_days};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

mod diff;
mod rows;

pub use diff::{Change, depth, diff};
pub use rows::{rel_path, rows, rows_to};

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

impl Sizes {
    /// The sizes in a node's totals.
    #[must_use]
    pub fn of(t: &Totals) -> Sizes {
        Sizes {
            disk: t.alloc,
            apparent: t.apparent,
            entries: t.items,
        }
    }
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

/// What a finished scan found below its root, read from the tree for its [`Record`]. Owned, so
/// it can be read under a lock and recorded after.
#[derive(Clone, Debug)]
pub struct Scanned {
    pub root: PathBuf,
    pub complete: bool,
    pub total: Sizes,
    pub errors: u64,
    pub rows: SizeMap,
}

impl Scanned {
    /// The scan below `root`, `complete` or not, with its [`rows`].
    #[must_use]
    pub fn read(tree: &Tree, root: Ix, complete: bool) -> Scanned {
        let t = tree.total(root);
        Scanned {
            root: tree.path_buf(root),
            complete,
            total: Sizes::of(&t),
            errors: t.errors,
            rows: rows(tree, root),
        }
    }

    /// The record of this scan, with the key of its `options`.
    #[must_use]
    pub fn record<'a>(&'a self, options: &'a str, started_at: i64, finished_at: i64) -> Record<'a> {
        Record {
            root: &self.root,
            options,
            started_at,
            finished_at,
            complete: self.complete,
            total: self.total,
            errors: self.errors,
            rows: &self.rows,
        }
    }
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

/// The key under which scans made with `job` are compared, `measured` when shared extents
/// were measured: scans that count other data (other mounts, shared extents measured or not)
/// are never compared with each other.
#[must_use]
pub fn options_key(job: &JobOptions, measured: bool) -> String {
    let JobOptions {
        cross_mounts,
        network,
        ..
    } = job;
    format!("cross_mounts={cross_mounts} network={network} measured={measured}")
}

/// Seconds since the Unix epoch as a UTC date ("2026-10-18").
#[must_use]
pub fn day(secs: i64) -> String {
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    format!("{y:04}-{m:02}-{d:02}")
}

/// Seconds since the Unix epoch as a UTC date and time ("2026-10-24 09:42 UTC").
#[must_use]
pub fn date_time(secs: i64) -> String {
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let rem = secs.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02} UTC",
        rem / 3600,
        rem % 3600 / 60
    )
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
    #[must_use]
    pub fn get(&self, path: &Path) -> Option<Sizes> {
        let rel = path.strip_prefix(&self.root).ok()?;
        self.sizes.get(rel.as_os_str().as_encoded_bytes()).copied()
    }

    /// The last scan of `root` with `options` that finished before `before`, in the database
    /// at `db`, if there is one.
    ///
    /// # Errors
    ///
    /// When the database exists but cannot be opened, migrated or read.
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

/// `history.sqlite3` in the state directory ([`dfdu_rules::dirs`]).
#[must_use]
pub fn default_path() -> Option<PathBuf> {
    dfdu_rules::dirs::history_path()
}

impl History {
    /// Opens or creates the database at `path` (and its directory).
    ///
    /// # Errors
    ///
    /// When the directory cannot be created, the database cannot be opened or migrated, or it
    /// has a schema newer than this version reads.
    pub fn open(path: &Path) -> io::Result<History> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path).map_err(err)?;
        Self::setup(conn)
    }

    /// A database in memory, for tests.
    ///
    /// # Errors
    ///
    /// When `SQLite` cannot create or set up the database.
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
        // In WAL mode a commit then only appends to the log and checkpoints sync it: a power
        // loss can drop the last scans recorded but never damages the database. Without the
        // sync in every commit the write lock is held for a moment instead of a flush, which on
        // Windows is long enough that connections waiting their turn ran out the busy timeout.
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(err)?;
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

    /// Brings the schema up to [`SCHEMA`]. A database already there is only read, so opening one
    /// does not queue for the write lock behind the connections recording.
    fn migrate(&mut self) -> io::Result<()> {
        if Self::version(&self.conn)? == SCHEMA {
            return Ok(());
        }
        let tx = self.write()?;
        // Read again under the lock: another connection may have migrated in between.
        let version = Self::version(&tx)?;
        if version == SCHEMA {
            return Ok(());
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

    /// The schema version of the database, refused when newer than [`SCHEMA`].
    fn version(conn: &Connection) -> io::Result<i64> {
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(err)?;
        if version > SCHEMA {
            return Err(io::Error::other(format!(
                "the history database has schema {version}, written by a newer dfdu (this one \
                 reads {SCHEMA})"
            )));
        }
        Ok(version)
    }

    /// Records a scan in one transaction and drops the oldest scans of its root past
    /// [`KEEP_SCANS`]. Returns its id.
    ///
    /// # Errors
    ///
    /// When the database cannot be written; nothing of the scan is kept then.
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
                r.total.disk.cast_signed(),
                r.total.apparent.cast_signed(),
                r.total.entries.cast_signed(),
                r.errors.cast_signed(),
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
                    s.disk.cast_signed(),
                    s.apparent.cast_signed(),
                    s.entries.cast_signed()
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
                disk: r.get::<_, i64>(6)?.cast_unsigned(),
                apparent: r.get::<_, i64>(7)?.cast_unsigned(),
                entries: r.get::<_, i64>(8)?.cast_unsigned(),
            },
            errors: r.get::<_, i64>(9)?.cast_unsigned(),
        })
    }

    const COLUMNS: &str =
        "id, root, options, started_at, finished_at, complete, disk, apparent, entries, errors";

    /// The scans of `root` (every root when `None`), newest first.
    ///
    /// # Errors
    ///
    /// When the database cannot be read.
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
    ///
    /// # Errors
    ///
    /// When the database cannot be read.
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
    ///
    /// # Errors
    ///
    /// When the database cannot be read.
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
    ///
    /// # Errors
    ///
    /// When there is no scan `id` or the database cannot be read.
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
                        disk: r.get::<_, i64>(1)?.cast_unsigned(),
                        apparent: r.get::<_, i64>(2)?.cast_unsigned(),
                        entries: r.get::<_, i64>(3)?.cast_unsigned(),
                    },
                ))
            })
            .map_err(err)?;
        let mut out = SizeMap::new();
        for row in rows {
            let (full, s) = row.map_err(err)?;
            let rel = full.strip_prefix(root.as_slice()).map_or(&full[..], |r| {
                r.strip_prefix(&[std::path::MAIN_SEPARATOR as u8])
                    .unwrap_or(r)
            });
            out.insert(rel.to_vec(), s);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests;

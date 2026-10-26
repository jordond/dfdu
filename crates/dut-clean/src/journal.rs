//! Per-session deletion journal (`docs/data-pipeline.md` §9.2): one JSON object per line, each
//! with a CRC-32C, `intent` written and synced before the item is touched. The file is held
//! under an exclusive lock for the session, so another instance never treats it as abandoned.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use dut_tree::{Kind, name_bytes};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use crate::{DeleteResult, Mode, PlanItem};

/// `$DUT_STATE_DIR`, else the platform state directory, plus `dut/ops`.
pub fn default_journal_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("DUT_STATE_DIR") {
        return Some(PathBuf::from(d).join("ops"));
    }
    // The CLI convention: XDG directories on Linux and macOS, Known Folders on Windows (which
    // has no state directory, so the data directory is used there).
    use etcetera::BaseStrategy;
    let dirs = etcetera::choose_base_strategy().ok()?;
    let base = dirs.state_dir().unwrap_or_else(|| dirs.data_dir());
    Some(base.join("dut").join("ops"))
}

pub struct Journal {
    file: File,
    path: PathBuf,
    last_sync: Instant,
    dirty: bool,
}

const SYNC_EVERY: Duration = Duration::from_millis(100);

/// One journal record. Each line is `{"rec": <record>, "crc": "<hex>"}`, where the CRC-32C covers
/// the bytes of `<record>` exactly as written, so a damaged or torn line is detected and a
/// record with fields this version does not know still checks.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "kebab-case")]
enum Record {
    Session {
        pid: u32,
    },
    Intent {
        #[serde(flatten)]
        item: ItemRec,
        mode: String,
    },
    Done {
        #[serde(flatten)]
        item: ItemRec,
    },
    Failed {
        #[serde(flatten)]
        item: ItemRec,
        detail: String,
    },
}

impl Record {
    fn kind(&self) -> &'static str {
        match self {
            Record::Session { .. } => "session",
            Record::Intent { .. } => "intent",
            Record::Done { .. } => "done",
            Record::Failed { .. } => "failed",
        }
    }
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct ItemRec {
    plan: u64,
    /// For people reading the journal; `path_hex` is the exact path.
    path: String,
    path_hex: String,
    dev: u64,
    ino: u64,
    kind: String,
    size: u64,
    mtime: i64,
    /// btrfs: the uuid of the subvolume holding the item, in intent records. Subvolume `st_dev`
    /// numbers can change between mounts, so recovery matches the inode within this
    /// subvolume (`docs/btrfs.md` §3.6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    subvol_uuid: Option<String>,
}

impl ItemRec {
    fn of(plan: u64, item: &PlanItem) -> Self {
        let id = &item.identity;
        ItemRec {
            plan,
            path: item.path.display().to_string(),
            path_hex: hex::encode(name_bytes(item.path.as_os_str())),
            dev: id.dev,
            ino: id.ino,
            kind: kind_str(id.kind).to_string(),
            size: id.apparent,
            mtime: id.mtime,
            subvol_uuid: None,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Line<'a> {
    #[serde(borrow)]
    rec: &'a RawValue,
    crc: String,
}

fn kind_str(k: Kind) -> &'static str {
    match k {
        Kind::File => "file",
        Kind::Dir => "dir",
        Kind::Symlink => "symlink",
        Kind::Other => "other",
    }
}

impl Journal {
    /// Creates this session's journal in `dir`.
    pub fn create(dir: &Path) -> io::Result<Journal> {
        fs::create_dir_all(dir)?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let name = format!(
            "{}-{}-{}.jsonl",
            now.as_secs(),
            now.subsec_nanos(),
            std::process::id()
        );
        let path = dir.join(name);
        // Read access as well as append: Windows requires read or write access on the handle to
        // lock it, and append-only grants neither.
        let file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .append(true)
            .open(&path)?;
        file.lock()?;
        let mut j = Journal {
            file,
            path,
            last_sync: Instant::now(),
            dirty: false,
        };
        j.write(Record::Session {
            pid: std::process::id(),
        })?;
        j.sync()?;
        Ok(j)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn write(&mut self, rec: Record) -> io::Result<()> {
        let body = serde_json::value::to_raw_value(&rec).map_err(io::Error::other)?;
        let crc = format!("{:08x}", crc32c::crc32c(body.get().as_bytes()));
        let mut line =
            serde_json::to_string(&Line { rec: &body, crc }).map_err(io::Error::other)?;
        line.push('\n');
        self.file.write_all(line.as_bytes())?;
        self.dirty = true;
        Ok(())
    }

    /// Records the intent to act on `item` and syncs it to disk before returning.
    pub fn intent(&mut self, plan: u64, mode: Mode, item: &PlanItem) -> io::Result<()> {
        let mode = match mode {
            Mode::Trash => "trash",
            Mode::Permanent => "delete",
        };
        let rec = ItemRec {
            subvol_uuid: crate::verify::subvolume_uuid(&item.path, item.identity.dev),
            ..ItemRec::of(plan, item)
        };
        self.write(Record::Intent {
            item: rec,
            mode: mode.into(),
        })?;
        self.sync()
    }

    /// Records the outcome; synced at least every 100 ms and at the end of a plan.
    pub fn result(&mut self, plan: u64, item: &PlanItem, result: &DeleteResult) -> io::Result<()> {
        let item = ItemRec::of(plan, item);
        self.write(match result {
            DeleteResult::Deleted => Record::Done { item },
            other => Record::Failed {
                item,
                detail: format!("{other:?}"),
            },
        })?;
        if self.last_sync.elapsed() >= SYNC_EVERY {
            self.sync()?;
        }
        Ok(())
    }

    pub fn sync(&mut self) -> io::Result<()> {
        if self.dirty {
            self.file.sync_data()?;
            self.dirty = false;
        }
        self.last_sync = Instant::now();
        Ok(())
    }
}

/// Parses a journal line and checks its CRC. Returns the record type, or `None` for a damaged
/// or torn line.
pub fn check_line(line: &str) -> Option<&'static str> {
    let l: Line = serde_json::from_str(line).ok()?;
    let crc = u32::from_str_radix(&l.crc, 16).ok()?;
    if crc != crc32c::crc32c(l.rec.get().as_bytes()) {
        return None;
    }
    let rec: Record = serde_json::from_str(l.rec.get()).ok()?;
    Some(rec.kind())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Identity;
    use dut_tree::NodeRef;

    #[test]
    fn lines_carry_valid_crc_and_detect_damage() {
        let dir = tempfile::tempdir().unwrap();
        let mut j = Journal::create(dir.path()).unwrap();
        let mut tree = dut_tree::Tree::new();
        let meta = dut_tree::Meta {
            kind: Kind::Dir,
            apparent: 0,
            alloc: 0,
            mtime: 0,
            dev: 0,
            ino: 0,
            nlink: 1,
            mount: 0,
        };
        let root = tree.new_root(b"/x", &meta, true);
        let node: NodeRef = tree.node_ref(root);
        let item = PlanItem {
            node,
            path: PathBuf::from("/tmp/a \"quoted\"\nname"),
            identity: Identity {
                dev: 1,
                ino: 2,
                kind: Kind::File,
                apparent: 3,
                mtime: 4,
            },
            bytes: 4096,
        };
        j.intent(7, Mode::Trash, &item).unwrap();
        j.result(7, &item, &DeleteResult::Deleted).unwrap();
        j.sync().unwrap();
        let path = j.path().to_path_buf();
        // Windows locks are mandatory: the file can be read only after the journal is closed.
        drop(j);
        let text = fs::read_to_string(&path).unwrap();
        let kinds: Vec<_> = text.lines().map(|l| check_line(l).unwrap()).collect();
        assert_eq!(kinds, ["session", "intent", "done"]);
        let damaged = text
            .lines()
            .nth(1)
            .unwrap()
            .replace("\"plan\":7", "\"plan\":8");
        assert!(check_line(&damaged).is_none());
        let torn = &text.lines().nth(2).unwrap()[..20];
        assert!(check_line(torn).is_none());
        // A field from a newer version: the CRC covers the bytes as written, not a re-encoding.
        let rec = r#"{"t":"session","pid":1,"host":"x"}"#;
        let newer = format!(
            r#"{{"rec":{rec},"crc":"{:08x}"}}"#,
            crc32c::crc32c(rec.as_bytes())
        );
        assert_eq!(check_line(&newer), Some("session"));
    }
}

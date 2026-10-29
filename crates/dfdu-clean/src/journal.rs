//! Per-session deletion journal: one JSON object per line, each with a CRC-32C, `intent` written
//! and synced before the item is touched. The file is held under an exclusive lock for the
//! session, so another instance never treats it as abandoned.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use dfdu_tree::{Kind, name_bytes, name_os};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use crate::{DeleteResult, Mode, PlanItem};

/// Where deletion journals are kept: `ops` in the state directory ([`dfdu_rules::dirs`]).
#[must_use]
pub fn default_journal_dir() -> Option<PathBuf> {
    dfdu_rules::dirs::journal_dir()
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
pub(crate) enum Record {
    Session {
        pid: u32,
    },
    Intent {
        #[serde(flatten)]
        item: ItemRec,
        /// A mode this version does not know makes the record unreadable, like an unknown
        /// record type.
        mode: Mode,
        /// `stage`: where the item is renamed to, as `path_hex` is for the item.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stage_hex: Option<String>,
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
    /// Before a staged item's background delete starts.
    CommitIntent {
        #[serde(flatten)]
        item: ItemRec,
    },
    CommitDone {
        #[serde(flatten)]
        item: ItemRec,
    },
    /// An undo or a recovery put the item back, at `to_hex`.
    Restored {
        #[serde(flatten)]
        item: ItemRec,
        to_hex: String,
    },
    /// Before a native cleanup command runs, and after.
    Native {
        id: u64,
        argv: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
    },
    NativeDone {
        id: u64,
        result: String,
    },
}

impl Record {
    fn kind(&self) -> &'static str {
        match self {
            Record::Session { .. } => "session",
            Record::Intent { .. } => "intent",
            Record::Done { .. } => "done",
            Record::Failed { .. } => "failed",
            Record::CommitIntent { .. } => "commit-intent",
            Record::CommitDone { .. } => "commit-done",
            Record::Restored { .. } => "restored",
            Record::Native { .. } => "native",
            Record::NativeDone { .. } => "native-done",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct ItemRec {
    pub plan: u64,
    /// For people reading the journal; `path_hex` is the exact path.
    pub path: String,
    pub path_hex: String,
    pub dev: u64,
    pub ino: u64,
    #[serde(with = "kind_name")]
    pub kind: Kind,
    pub size: u64,
    pub mtime: i64,
    /// btrfs: the uuid of the subvolume holding the item, in intent records. Subvolume `st_dev`
    /// numbers can change between mounts, so recovery matches the inode within this subvolume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    subvol_uuid: Option<String>,
}

impl ItemRec {
    fn of(plan: u64, item: &PlanItem) -> Self {
        let id = &item.identity;
        ItemRec {
            plan,
            path: item.path.display().to_string(),
            path_hex: hex_path(&item.path),
            dev: id.dev,
            ino: id.ino,
            kind: id.kind,
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

pub(crate) fn hex_path(p: &Path) -> String {
    hex::encode(name_bytes(p.as_os_str()))
}

pub(crate) fn path_of_hex(h: &str) -> Option<PathBuf> {
    let b = hex::decode(h).ok()?;
    Some(PathBuf::from(name_os(&b).into_owned()))
}

/// An item's kind as the journal names it; a name this version does not know reads as
/// [`Kind::Other`].
mod kind_name {
    use dfdu_tree::Kind;
    use serde::{Deserialize, Deserializer, Serializer};

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "serde's `with` attribute passes the field by reference"
    )]
    pub fn serialize<S: Serializer>(k: &Kind, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(match k {
            Kind::File => "file",
            Kind::Dir => "dir",
            Kind::Symlink => "symlink",
            Kind::Other => "other",
        })
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Kind, D::Error> {
        Ok(match String::deserialize(d)?.as_str() {
            "file" => Kind::File,
            "dir" => Kind::Dir,
            "symlink" => Kind::Symlink,
            _ => Kind::Other,
        })
    }
}

impl Journal {
    /// Creates this session's journal in `dir`.
    ///
    /// # Errors
    ///
    /// `dir` or the journal file could not be created.
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
        j.write(&Record::Session {
            pid: std::process::id(),
        })?;
        j.sync()?;
        Ok(j)
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The session id: the journal's file name without `.jsonl`, also the name of the
    /// session's staging directories.
    #[must_use]
    pub fn session(&self) -> String {
        self.path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// Continues an earlier session's journal, whose lock the caller holds (recovery).
    pub(crate) fn resume(file: File, path: PathBuf) -> Journal {
        Journal {
            file,
            path,
            last_sync: Instant::now(),
            dirty: false,
        }
    }

    pub(crate) fn write(&mut self, rec: &Record) -> io::Result<()> {
        let body = serde_json::value::to_raw_value(rec).map_err(io::Error::other)?;
        let crc = format!("{:08x}", crc32c::crc32c(body.get().as_bytes()));
        let mut line =
            serde_json::to_string(&Line { rec: &body, crc }).map_err(io::Error::other)?;
        line.push('\n');
        self.file.write_all(line.as_bytes())?;
        self.dirty = true;
        Ok(())
    }

    /// Records the intent to act on `item` (to stage it at `stage`) and syncs it to disk
    /// before returning.
    ///
    /// # Errors
    ///
    /// The journal could not be written or synced.
    pub fn intent(
        &mut self,
        plan: u64,
        mode: Mode,
        item: &PlanItem,
        stage: Option<&Path>,
    ) -> io::Result<()> {
        let rec = ItemRec {
            subvol_uuid: crate::verify::subvolume_uuid(&item.path, item.identity.dev),
            ..ItemRec::of(plan, item)
        };
        self.write(&Record::Intent {
            item: rec,
            mode,
            stage_hex: stage.map(hex_path),
        })?;
        self.sync()
    }

    /// Before a staged item's background delete starts; synced.
    ///
    /// # Errors
    ///
    /// The journal could not be written or synced.
    pub fn commit_intent(&mut self, plan: u64, item: &PlanItem) -> io::Result<()> {
        self.write(&Record::CommitIntent {
            item: ItemRec::of(plan, item),
        })?;
        self.sync()
    }

    /// After a staged item's background delete; synced.
    ///
    /// # Errors
    ///
    /// The journal could not be written or synced.
    pub fn commit_done(&mut self, plan: u64, item: &PlanItem) -> io::Result<()> {
        self.write(&Record::CommitDone {
            item: ItemRec::of(plan, item),
        })?;
        self.sync()
    }

    /// After a staged item was put back at `to`; synced.
    ///
    /// # Errors
    ///
    /// The journal could not be written or synced.
    pub fn restored(&mut self, plan: u64, item: &PlanItem, to: &Path) -> io::Result<()> {
        self.write(&Record::Restored {
            item: ItemRec::of(plan, item),
            to_hex: hex_path(to),
        })?;
        self.sync()
    }

    /// Records the outcome; synced at least every 100 ms and at the end of a plan.
    ///
    /// # Errors
    ///
    /// The journal could not be written or synced.
    pub fn result(&mut self, plan: u64, item: &PlanItem, result: &DeleteResult) -> io::Result<()> {
        let item = ItemRec::of(plan, item);
        self.write(&match result {
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

    /// Before a native command runs; synced.
    ///
    /// # Errors
    ///
    /// The journal could not be written or synced.
    pub fn native(&mut self, req: &crate::NativeReq) -> io::Result<()> {
        self.write(&Record::Native {
            id: req.id,
            argv: req.command.clone(),
            cwd: req.cwd.as_ref().map(|p| p.display().to_string()),
        })?;
        self.sync()
    }

    /// After a native command ran, with its outcome; synced.
    ///
    /// # Errors
    ///
    /// The journal could not be written or synced.
    pub fn native_done(&mut self, id: u64, result: &Result<(), String>) -> io::Result<()> {
        self.write(&Record::NativeDone {
            id,
            result: match result {
                Ok(()) => "ok".into(),
                Err(e) => e.clone(),
            },
        })?;
        self.sync()
    }

    /// Syncs what was written since the last sync to disk.
    ///
    /// # Errors
    ///
    /// The journal could not be synced.
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
#[must_use]
pub fn check_line(line: &str) -> Option<&'static str> {
    parse_line(line).map(|r| r.kind())
}

/// A journal line's record, or `None` for a damaged or torn line or a record type this version
/// does not know.
pub(crate) fn parse_line(line: &str) -> Option<Record> {
    let l: Line = serde_json::from_str(line).ok()?;
    let crc = u32::from_str_radix(&l.crc, 16).ok()?;
    if crc != crc32c::crc32c(l.rec.get().as_bytes()) {
        return None;
    }
    serde_json::from_str(l.rec.get()).ok()
}

#[cfg(test)]
mod tests;

//! Per-session deletion journal (`docs/data-pipeline.md` §9.2): one JSON object per line, each
//! with a CRC-32C, `intent` written and synced before the item is touched. The file is held
//! under an exclusive lock for the session, so another instance never treats it as abandoned.

use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use dut_tree::Kind;

use crate::crc::crc32c;
use crate::{DeleteResult, Mode, PlanItem};

/// `$DUT_STATE_DIR`, else the platform state directory, plus `dut/ops`.
pub fn default_journal_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("DUT_STATE_DIR") {
        return Some(PathBuf::from(d).join("ops"));
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let base = if cfg!(target_os = "macos") {
        home.map(|h| h.join("Library/Application Support"))
    } else if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| home.map(|h| h.join(".local/state")))
    };
    base.map(|b| b.join("dut").join("ops"))
}

pub struct Journal {
    file: File,
    path: PathBuf,
    last_sync: Instant,
    dirty: bool,
}

const SYNC_EVERY: Duration = Duration::from_millis(100);

fn json_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
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
        j.write_line("session", &format!("\"pid\":{}", std::process::id()))?;
        j.sync()?;
        Ok(j)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn write_line(&mut self, kind: &str, fields: &str) -> io::Result<()> {
        let mut body = String::new();
        body.push_str("{\"t\":");
        json_str(&mut body, kind);
        if !fields.is_empty() {
            body.push(',');
            body.push_str(fields);
        }
        let crc = crc32c(body.as_bytes());
        let line = format!("{body},\"crc\":\"{crc:08x}\"}}\n");
        self.file.write_all(line.as_bytes())?;
        self.dirty = true;
        Ok(())
    }

    fn item_fields(plan: u64, item: &PlanItem) -> String {
        let mut f = format!("\"plan\":{plan},\"path\":");
        json_str(&mut f, &item.path.to_string_lossy());
        let id = &item.identity;
        let _ = write!(
            f,
            ",\"path_hex\":\"{}\",\"dev\":{},\"ino\":{},\"kind\":\"{}\",\"size\":{},\"mtime\":{}",
            hex(item.path.as_os_str().as_encoded_bytes()),
            id.dev,
            id.ino,
            kind_str(id.kind),
            id.apparent,
            id.mtime
        );
        f
    }

    /// Records the intent to act on `item` and syncs it to disk before returning.
    pub fn intent(&mut self, plan: u64, mode: Mode, item: &PlanItem) -> io::Result<()> {
        let mode = match mode {
            Mode::Trash => "trash",
            Mode::Permanent => "delete",
        };
        let fields = format!("{},\"mode\":\"{mode}\"", Self::item_fields(plan, item));
        self.write_line("intent", &fields)?;
        self.sync()
    }

    /// Records the outcome; synced at least every 100 ms and at the end of a plan.
    pub fn result(&mut self, plan: u64, item: &PlanItem, result: &DeleteResult) -> io::Result<()> {
        let (t, detail) = match result {
            DeleteResult::Deleted => ("done", String::new()),
            other => ("failed", format!("{other:?}")),
        };
        let mut fields = Self::item_fields(plan, item);
        if !detail.is_empty() {
            fields.push_str(",\"detail\":");
            json_str(&mut fields, &detail);
        }
        self.write_line(t, &fields)?;
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
pub fn check_line(line: &str) -> Option<&str> {
    let body_end = line.rfind(",\"crc\":\"")?;
    let (body, tail) = line.split_at(body_end);
    let crc = tail.strip_prefix(",\"crc\":\"")?.strip_suffix("\"}")?;
    if u32::from_str_radix(crc, 16).ok()? != crc32c(body.as_bytes()) {
        return None;
    }
    let t = body.strip_prefix("{\"t\":\"")?;
    Some(&t[..t.find('"')?])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Identity;
    use dut_tree::NodeRef;

    #[test]
    fn lines_carry_valid_crc_and_detect_damage() {
        let dir = std::env::temp_dir().join(format!("dut-journal-test-{}", std::process::id()));
        let mut j = Journal::create(&dir).unwrap();
        let mut tree = dut_tree::Tree::new();
        let meta = dut_tree::Meta {
            kind: Kind::Dir,
            apparent: 0,
            alloc: 0,
            mtime: 0,
            dev: 0,
            ino: 0,
            nlink: 1,
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
        let _ = fs::remove_dir_all(&dir);
    }
}

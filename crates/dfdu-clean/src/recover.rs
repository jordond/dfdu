//! What earlier sessions left unfinished: items staged and not yet
//! deleted, staged deletes cut short, and intents with no result. Only journals whose lock can
//! be taken are read, which means their process has exited; a running instance's journal and
//! staging directories are never touched. The locks are held for as long as the
//! [`Recovery`] lives, and what is done is recorded in the same journal.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

use dfdu_tree::Kind;

use crate::Mode;
use crate::journal::{ItemRec, Journal, Record, hex_path, parse_line, path_of_hex};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pending {
    /// Staged and not deleted yet: it can be restored or deleted.
    Staged { stage: PathBuf },
    /// Its background delete started and did not finish: part of it is gone, so it can only be
    /// deleted, never restored as if it were whole.
    PartlyDeleted { stage: PathBuf },
    /// The session ended between the intent and the result. `at_original` says whether
    /// something is still at the item's path; with the trash, the item may be there.
    Interrupted { mode: Mode, at_original: bool },
    /// Interrupted on its way to the trash, and found there: it can be put back.
    Trashed,
}

#[derive(Clone, Debug)]
pub struct Unfinished {
    pub journal: PathBuf,
    pub path: PathBuf,
    pub kind: Kind,
    pub state: Pending,
    session: usize,
    rec: ItemRec,
}

impl Unfinished {
    /// How the TUI and `clean` describe it.
    #[must_use]
    pub fn describe(&self) -> String {
        let p = self.path.display();
        match &self.state {
            Pending::Staged { stage } => format!("{p}: staged at {}", stage.display()),
            Pending::PartlyDeleted { stage } => {
                format!("{p}: partly deleted, the rest is at {}", stage.display())
            }
            Pending::Interrupted { mode, at_original } => format!(
                "{p}: interrupted ({}); {}",
                mode.journal_name(),
                if *at_original {
                    "something is still at its path"
                } else if *mode == Mode::Trash {
                    "it is no longer at its path, and was not found in the trash"
                } else {
                    "it is no longer at its path"
                }
            ),
            Pending::Trashed => format!("{p}: interrupted (trash); it is in the trash"),
        }
    }
}

/// An item's record, its state so far (`None` once nothing is left to do) and its staging
/// path.
type OpenItem = (ItemRec, Option<Pending>, Option<PathBuf>);

struct Session {
    journal: Journal,
    /// When the session started (seconds since the epoch, from the journal's name), before
    /// anything it trashed went to the trash.
    started: i64,
    /// A line other than the last failed its check: only reported, never acted on.
    damaged: bool,
}

pub struct Recovery {
    sessions: Vec<Session>,
    pub items: Vec<Unfinished>,
    /// Damaged journals and torn last lines, for the user.
    pub notes: Vec<String>,
}

/// Reads every finished session's journal in `dir`.
///
/// # Errors
///
/// `dir` could not be listed. A journal that cannot be read or is damaged is a note in the
/// result instead.
pub fn scan(dir: &Path) -> io::Result<Recovery> {
    let mut r = Recovery {
        sessions: vec![],
        items: vec![],
        notes: vec![],
    };
    let mut paths: Vec<PathBuf> = match fs::read_dir(dir) {
        Ok(rd) => rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
            .collect(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(r),
        Err(e) => return Err(e),
    };
    paths.sort();
    for path in paths {
        let file = OpenOptions::new().read(true).append(true).open(&path)?;
        match file.try_lock() {
            Ok(()) => {}
            // A running session.
            Err(TryLockError::WouldBlock) => continue,
            Err(TryLockError::Error(e)) => return Err(e),
        }
        r.read_session(path, file)?;
    }
    Ok(r)
}

impl Recovery {
    fn read_session(&mut self, path: PathBuf, file: File) -> io::Result<()> {
        let text = fs::read_to_string(&path)?;
        let lines: Vec<&str> = text.lines().collect();
        let mut damaged = false;
        // By (plan, path).
        let mut open: HashMap<(u64, String), OpenItem> = HashMap::new();
        for (i, line) in lines.iter().enumerate() {
            let Some(rec) = parse_line(line) else {
                if i + 1 == lines.len() {
                    self.notes.push(format!(
                        "{}: the last record is incomplete and was ignored",
                        path.display()
                    ));
                } else {
                    damaged = true;
                }
                continue;
            };
            let key = |it: &ItemRec| (it.plan, it.path_hex.clone());
            match rec {
                Record::Session { .. } | Record::Native { .. } | Record::NativeDone { .. } => {}
                Record::Intent {
                    item,
                    mode,
                    stage_hex,
                } => {
                    let stage = stage_hex.as_deref().and_then(path_of_hex);
                    let at_original = path_of_hex(&item.path_hex)
                        .is_some_and(|p| fs::symlink_metadata(p).is_ok());
                    let pending = Pending::Interrupted { mode, at_original };
                    open.insert(key(&item), (item, Some(pending), stage));
                }
                Record::Done { item } => {
                    if let Some(e) = open.get_mut(&key(&item)) {
                        e.1 = e.2.clone().map(|stage| Pending::Staged { stage });
                    }
                }
                Record::CommitIntent { item } => {
                    if let Some(e) = open.get_mut(&key(&item)) {
                        e.1 = e.2.clone().map(|stage| Pending::PartlyDeleted { stage });
                    }
                }
                Record::Failed { item, .. }
                | Record::CommitDone { item }
                | Record::Restored { item, .. } => {
                    open.remove(&key(&item));
                }
            }
        }
        if damaged {
            self.notes.push(format!(
                "{}: damaged; its items are reported but not acted on",
                path.display()
            ));
        }
        let session = self.sessions.len();
        // Journals are named `<seconds>-<nanoseconds>-<pid>.jsonl`.
        let started = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.split('-').next())
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0);
        let mut items: Vec<Unfinished> = open
            .into_values()
            .filter_map(|(rec, state, stage)| {
                let mut state = state?;
                // Interrupted while staging: the rename either happened or it did not.
                if let (Pending::Interrupted { mode, .. }, Some(s)) = (&state, &stage)
                    && *mode == Mode::Stage
                    && fs::symlink_metadata(s).is_ok()
                {
                    state = Pending::Staged { stage: s.clone() };
                }
                // Interrupted on its way to the trash: it may be there.
                if let Pending::Interrupted {
                    mode,
                    at_original: false,
                } = &state
                    && *mode == Mode::Trash
                    && let Some(path) = path_of_hex(&rec.path_hex)
                    && crate::stage::in_trash(&path, &identity(&rec), started).is_ok_and(|t| t)
                {
                    state = Pending::Trashed;
                }
                Some(Unfinished {
                    journal: path.clone(),
                    path: path_of_hex(&rec.path_hex)?,
                    kind: rec.kind,
                    state,
                    session,
                    rec,
                })
            })
            .collect();
        items.sort_by(|a, b| a.path.cmp(&b.path));
        self.items.extend(items);
        self.sessions.push(Session {
            journal: Journal::resume(file, path),
            started,
            damaged,
        });
        Ok(())
    }

    fn checked(&self, i: usize) -> io::Result<&Unfinished> {
        let u = self.items.get(i).ok_or_else(|| refused("no such item"))?;
        if self.sessions[u.session].damaged {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{} is damaged; nothing is done from it",
                    u.journal.display()
                ),
            ));
        }
        Ok(u)
    }

    /// Deletes what is left of a staged item.
    ///
    /// # Errors
    ///
    /// There is no item `i`, it is not staged, its journal could not be written, or the delete
    /// failed.
    pub fn finish(&mut self, i: usize) -> io::Result<()> {
        let u = self.checked(i)?.clone();
        let stage = match &u.state {
            Pending::Staged { stage } | Pending::PartlyDeleted { stage } => stage,
            Pending::Interrupted { .. } => return Err(refused("not staged; nothing to finish")),
            Pending::Trashed => {
                return Err(refused("it is in the trash: empty the trash to delete it"));
            }
        };
        let j = &mut self.sessions[u.session].journal;
        if matches!(u.state, Pending::Staged { .. }) {
            record(
                j,
                &Record::CommitIntent {
                    item: u.rec.clone(),
                },
            )?;
        }
        match crate::remove(stage, u.kind) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(io::Error::new(
                    e.kind(),
                    format!("{}: {e}", stage.display()),
                ));
            }
        }
        record(
            j,
            &Record::CommitDone {
                item: u.rec.clone(),
            },
        )?;
        if let Some(d) = stage.parent() {
            let _ = fs::remove_dir(d);
        }
        self.items.remove(i);
        Ok(())
    }

    /// Closes an interrupted item in its journal, once the user has seen what is at its path
    /// (or in the trash): nothing is done on disk.
    ///
    /// # Errors
    ///
    /// There is no item `i`, it is staged, or its journal could not be written.
    pub fn dismiss(&mut self, i: usize) -> io::Result<()> {
        let u = self.checked(i)?.clone();
        if !matches!(u.state, Pending::Interrupted { .. } | Pending::Trashed) {
            return Err(refused("staged; finish or restore it instead"));
        }
        record(
            &mut self.sessions[u.session].journal,
            &Record::Failed {
                item: u.rec.clone(),
                detail: "interrupted; dismissed in a later session".into(),
            },
        )?;
        self.items.remove(i);
        Ok(())
    }

    /// Puts an item found in the trash back at its path (refused if something is there now).
    /// Puts a staged item back at its path, or next to it as `<name>.restored-<n>` if
    /// something is there now. Refused for a partly deleted item.
    ///
    /// # Errors
    ///
    /// There is no item `i`, it cannot be restored (partly deleted, or something is at the path
    /// of an item in the trash), the move failed, or its journal could not be written.
    pub fn restore(&mut self, i: usize) -> io::Result<PathBuf> {
        let u = self.checked(i)?.clone();
        if u.state == Pending::Trashed {
            let started = self.sessions[u.session].started;
            let to = crate::stage::restore_from_trash(&u.path, &identity(&u.rec), started)?;
            record(
                &mut self.sessions[u.session].journal,
                &Record::Restored {
                    item: u.rec.clone(),
                    to_hex: hex_path(&to),
                },
            )?;
            self.items.remove(i);
            return Ok(to);
        }
        let Pending::Staged { stage } = &u.state else {
            return Err(refused(match u.state {
                Pending::PartlyDeleted { .. } => {
                    "partly deleted; it can only be finished, not restored"
                }
                _ => "not staged; nothing to restore",
            }));
        };
        let to = crate::stage::put_back(stage, &u.path)?;
        record(
            &mut self.sessions[u.session].journal,
            &Record::Restored {
                item: u.rec.clone(),
                to_hex: hex_path(&to),
            },
        )?;
        if let Some(d) = stage.parent() {
            let _ = fs::remove_dir(d);
        }
        self.items.remove(i);
        Ok(to)
    }
}

/// The identity the scan saw, as the journal recorded it.
fn identity(rec: &ItemRec) -> crate::Identity {
    crate::Identity {
        dev: rec.dev,
        ino: rec.ino,
        kind: rec.kind,
        apparent: rec.size,
        mtime: rec.mtime,
    }
}

/// An action the item's state does not allow.
fn refused(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, why)
}

fn record(j: &mut Journal, rec: &Record) -> io::Result<()> {
    j.write(rec)
        .and_then(|()| j.sync())
        .map_err(|e| io::Error::new(e.kind(), format!("journal: {e}")))
}

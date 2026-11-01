//! Saving the tree to the cache: on a thread, again once it changed and stayed the same, and
//! cancelled on the way out.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::thread;
use std::time::Instant;

use crossbeam_channel::RecvTimeoutError;
use dfdu_engine::Engine;
use dfdu_store::cache::{self, Saved};
use dfdu_store::snapshot::Header;

use super::{CANCEL_WAIT, CacheLink, Cacher, RESAVE_QUIET, SavedAs, ended};
use crate::state::SessionState;

/// A save running on its thread.
pub(super) struct Writer {
    thread: thread::JoinHandle<std::io::Result<Saved>>,
    cancel: Arc<AtomicBool>,
    /// Disconnected once the thread is done: it holds the other end until then.
    done: crossbeam_channel::Receiver<()>,
}

impl Cacher {
    /// Saves the tree, at `version`, on a thread. True when a thread could not be started,
    /// which the status line says.
    pub(super) fn save<B: dfdu_scan::Backend>(
        &mut self,
        link: &CacheLink,
        s: &mut SessionState,
        engine: &Engine<B>,
        version: u64,
    ) -> bool {
        let Some(run) = &self.run else {
            return false;
        };
        let SavedAs::Save(full_scan) = run.saved_as else {
            return false;
        };
        let now = cache::now();
        let header = Header::new(
            now,
            link.slot.key.cross_mounts,
            link.slot.key.network,
            run.journal,
            full_scan,
        );
        let read = engine.reader();
        let cancel = Arc::new(AtomicBool::new(false));
        let (slot, stop) = (link.slot.clone(), cancel.clone());
        let (running, done) = crossbeam_channel::bounded(0);
        let spawned = thread::Builder::new()
            .name("dfdu-cache".into())
            .spawn(move || {
                let _running: crossbeam_channel::Sender<()> = running;
                let saved = slot.save(&header, |f| read(&mut |m| f(&m.tree)), &stop)?;
                if matches!(saved, Saved::Written { .. }) {
                    slot.sweep(now);
                }
                Ok(saved)
            });
        let writer = spawned.map(|thread| Writer {
            thread,
            cancel,
            done,
        });
        self.save_started(link, s, version, writer)
    }

    /// Takes in the start of a save of the tree at `version`. A thread that could not be
    /// started goes in the status line, and that version is not tried again: only a change to
    /// the tree is saved again. True when the status line changed.
    pub(super) fn save_started(
        &mut self,
        link: &CacheLink,
        s: &mut SessionState,
        version: u64,
        writer: std::io::Result<Writer>,
    ) -> bool {
        self.saved = Some(version);
        self.changed = None;
        match writer {
            Ok(w) => {
                self.writer = Some(w);
                false
            }
            Err(e) => {
                s.status = Some(format!(
                    "cache {}: cannot start a thread: {e}",
                    link.slot.path.display()
                ));
                true
            }
        }
    }

    /// Takes in the end of a save: a problem goes in the status line, and a tree that changed
    /// while it was written is saved again once it stays the same.
    pub(super) fn save_ended(&mut self, link: &CacheLink, s: &mut SessionState) -> bool {
        let Some(w) = self.writer.take_if(|w| w.thread.is_finished()) else {
            return false;
        };
        match w.thread.join() {
            Ok(Ok(Saved::Written { .. } | Saved::Cancelled)) => false,
            Ok(Ok(Saved::Changed)) => {
                self.saved = None;
                false
            }
            Ok(Err(e)) => {
                s.status = Some(format!("cache {}: {e}", link.slot.path.display()));
                true
            }
            Err(_) => {
                s.status = Some(format!(
                    "cache {}: the writer panicked",
                    link.slot.path.display()
                ));
                true
            }
        }
    }

    /// Saves again a tree that changed since it was saved, once it has stayed the same for
    /// [`RESAVE_QUIET`] and is still complete. True when the status line changed.
    pub(super) fn resave<B: dfdu_scan::Backend>(
        &mut self,
        link: &CacheLink,
        s: &mut SessionState,
        engine: &Engine<B>,
    ) -> bool {
        let worth = self
            .run
            .as_ref()
            .is_some_and(|r| matches!(r.saved_as, SavedAs::Save(_)));
        if !worth || self.writer.is_some() {
            return false;
        }
        let Some((true, version)) = ended(engine) else {
            self.changed = None;
            return false;
        };
        if self.saved == Some(version) {
            self.changed = None;
            return false;
        }
        match self.changed {
            Some((v, since)) if v == version => {
                if since.elapsed() >= RESAVE_QUIET {
                    return self.save(link, s, engine, version);
                }
            }
            _ => self.changed = Some((version, Instant::now())),
        }
        false
    }
}

impl Drop for Cacher {
    fn drop(&mut self) {
        let Some(w) = self.writer.take() else {
            return;
        };
        w.cancel.store(true, Relaxed);
        if w.done.recv_timeout(CANCEL_WAIT) == Err(RecvTimeoutError::Disconnected) {
            let _ = w.thread.join();
        }
    }
}

#[cfg(test)]
mod tests;

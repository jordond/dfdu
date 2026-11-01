//! The bridge from the engine to the window: a thread waits on the session's change channel and
//! asks egui for a frame when the model changed, so an idle window draws nothing. The change it
//! took is handed to the next frame, which passes it to the session
//! ([`dfdu_session::Session::note_change`]).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crossbeam_channel::Receiver;

/// The flag the bridge thread raises.
pub struct Bridge {
    changed: Arc<AtomicBool>,
}

impl Bridge {
    /// Starts the thread on `changes`. It ends when the engine drops its sender (on shutdown).
    pub fn start(changes: Receiver<()>, ctx: egui::Context) -> Self {
        let changed = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&changed);
        let spawned = std::thread::Builder::new()
            .name("dfdu-gui-changes".into())
            .spawn(move || {
                while changes.recv().is_ok() {
                    flag.store(true, Ordering::Release);
                    ctx.request_repaint();
                }
            });
        if let Err(e) = spawned {
            // Without the thread, the frames the session asks for still read the model.
            eprintln!("dfdu-gui: cannot start a thread: {e}");
        }
        Bridge { changed }
    }

    /// Whether the thread took a change since the last call.
    pub fn take(&self) -> bool {
        self.changed.swap(false, Ordering::AcqRel)
    }
}

//! The thread that reads the terminal's events for the browser ([`Reader`]). It is started
//! once and its events go to every browser loop the run opens, one after another. It can be
//! paused, so that while the terminal is handed to something else (ssh asking for a password)
//! no key is taken from it.

use std::io;
use std::thread;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, TryRecvError, bounded, unbounded};
use ratatui::crossterm::event::{self, Event};

/// How long the thread waits for an event before it looks for a pause.
const POLL: Duration = Duration::from_millis(250);

/// The reading thread's events, and the way to pause it.
pub(crate) struct Reader {
    events: Receiver<Event>,
    control: Sender<Pause>,
}

/// A pause asked of the thread: it says it stopped on `parked`, and reads again once
/// `resume`'s sender is dropped.
struct Pause {
    parked: Sender<()>,
    resume: Receiver<()>,
}

/// While held, the thread does not read the terminal.
pub(crate) struct Paused {
    _resume: Sender<()>,
}

impl Reader {
    /// Starts the thread.
    ///
    /// # Errors
    ///
    /// When the thread cannot be started.
    pub(crate) fn spawn() -> io::Result<Reader> {
        let (tx, events) = unbounded();
        let (control, pauses) = unbounded();
        thread::Builder::new()
            .name("dfdu-input".into())
            .spawn(move || read(&tx, &pauses))?;
        Ok(Reader { events, control })
    }

    /// The terminal's events. It closes when the terminal cannot be read any more.
    pub(crate) fn events(&self) -> &Receiver<Event> {
        &self.events
    }

    /// Stops the thread reading the terminal, and returns once it has, until the guard is
    /// dropped.
    pub(crate) fn pause(&self) -> Paused {
        let (parked_tx, parked) = bounded(1);
        let (resume, resume_rx) = bounded(0);
        let asked = self.control.send(Pause {
            parked: parked_tx,
            resume: resume_rx,
        });
        if asked.is_ok() {
            // An error: the thread ended, so it reads nothing either.
            let _ = parked.recv();
        }
        Paused { _resume: resume }
    }
}

/// The thread: sends each event to `tx` until the terminal cannot be read or nobody listens,
/// and stops while a pause from `pauses` is held.
fn read(tx: &Sender<Event>, pauses: &Receiver<Pause>) {
    loop {
        match pauses.try_recv() {
            Ok(p) => {
                let _ = p.parked.send(());
                // Returns once the guard is dropped.
                let _ = p.resume.recv();
                continue;
            }
            Err(TryRecvError::Disconnected) => return,
            Err(TryRecvError::Empty) => {}
        }
        match event::poll(POLL) {
            Ok(true) => match event::read() {
                Ok(ev) => {
                    if tx.send(ev).is_err() {
                        return;
                    }
                }
                Err(_) => return,
            },
            Ok(false) => {}
            Err(_) => return,
        }
    }
}

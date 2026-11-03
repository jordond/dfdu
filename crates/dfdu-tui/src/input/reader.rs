//! The thread that reads the terminal's events for the browser ([`Reader`]). It is started
//! once and its events go to every browser loop the run opens, one after another. It can be
//! paused, so that while the terminal is handed to something else (ssh asking for a password)
//! no key is taken from it. What it reads from is an [`EventSource`]: the terminal, or a fake
//! one in tests.

use std::io;
use std::thread;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, TryRecvError, bounded, unbounded};
use ratatui::crossterm::event::{self, Event};

/// How long the thread waits for an event before it looks for a pause.
const POLL: Duration = Duration::from_millis(250);

/// Where the thread reads events from.
pub(crate) trait EventSource: Send + 'static {
    /// Whether an event is ready, waiting at most `timeout`.
    ///
    /// # Errors
    ///
    /// When the source cannot be read any more.
    fn poll(&mut self, timeout: Duration) -> io::Result<bool>;
    /// The next event, once [`EventSource::poll`] said one is ready.
    ///
    /// # Errors
    ///
    /// When the source cannot be read any more.
    fn read(&mut self) -> io::Result<Event>;
}

/// The terminal's events, through crossterm.
struct Terminal;

impl EventSource for Terminal {
    fn poll(&mut self, timeout: Duration) -> io::Result<bool> {
        event::poll(timeout)
    }

    fn read(&mut self) -> io::Result<Event> {
        event::read()
    }
}

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
    /// Starts the thread on the terminal.
    ///
    /// # Errors
    ///
    /// When the thread cannot be started.
    pub(crate) fn spawn() -> io::Result<Reader> {
        Reader::spawn_on(Terminal, POLL)
    }

    /// Starts the thread on `source`, looking for a pause at least every `poll`.
    fn spawn_on(mut source: impl EventSource, poll: Duration) -> io::Result<Reader> {
        let (tx, events) = unbounded();
        let (control, pauses) = unbounded();
        thread::Builder::new()
            .name("dfdu-input".into())
            .spawn(move || read(&mut source, poll, &tx, &pauses))?;
        Ok(Reader { events, control })
    }

    /// The terminal's events. It closes when the terminal cannot be read any more.
    pub(crate) fn events(&self) -> &Receiver<Event> {
        &self.events
    }

    /// Drops the events read and not taken yet: typed for a browser that is gone.
    pub(crate) fn drain(&self) {
        while self.events.try_recv().is_ok() {}
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

/// The thread: sends each event of `source` to `tx` until it cannot be read or nobody
/// listens, and stops while a pause from `pauses` is held.
fn read(
    source: &mut impl EventSource,
    poll: Duration,
    tx: &Sender<Event>,
    pauses: &Receiver<Pause>,
) {
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
        match source.poll(poll) {
            Ok(true) => match source.read() {
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

#[cfg(test)]
mod tests {
    use crossbeam_channel::RecvTimeoutError;
    use ratatui::crossterm::event::{KeyCode, KeyEvent};

    use super::*;

    /// Events sent to a channel; it ends when the sender is dropped.
    struct Fake {
        rx: Receiver<Event>,
        next: Option<Event>,
    }

    impl EventSource for Fake {
        fn poll(&mut self, timeout: Duration) -> io::Result<bool> {
            if self.next.is_some() {
                return Ok(true);
            }
            match self.rx.recv_timeout(timeout) {
                Ok(ev) => {
                    self.next = Some(ev);
                    Ok(true)
                }
                Err(RecvTimeoutError::Timeout) => Ok(false),
                Err(RecvTimeoutError::Disconnected) => Err(io::Error::other("closed")),
            }
        }

        fn read(&mut self) -> io::Result<Event> {
            self.next
                .take()
                .ok_or_else(|| io::Error::other("nothing ready"))
        }
    }

    fn key(c: char) -> Event {
        Event::Key(KeyEvent::from(KeyCode::Char(c)))
    }

    const WAIT: Duration = Duration::from_secs(5);
    const SHORT: Duration = Duration::from_millis(5);

    fn reader() -> (Sender<Event>, Reader) {
        let (tx, rx) = unbounded();
        let reader = Reader::spawn_on(Fake { rx, next: None }, SHORT).expect("thread");
        (tx, reader)
    }

    #[test]
    fn a_paused_reader_takes_nothing_until_the_guard_is_dropped() {
        let (tx, reader) = reader();
        tx.send(key('a')).expect("send");
        assert_eq!(reader.events().recv_timeout(WAIT).ok(), Some(key('a')));

        // `pause` returns once the thread has parked: what is typed now is left alone.
        let paused = reader.pause();
        tx.send(key('b')).expect("send");
        let idle = reader.events().recv_timeout(SHORT * 20);
        assert!(idle.is_err(), "read while paused: {idle:?}");

        drop(paused);
        assert_eq!(reader.events().recv_timeout(WAIT).ok(), Some(key('b')));
    }

    #[test]
    fn drain_drops_what_was_read_and_not_taken() {
        let (tx, reader) = reader();
        tx.send(key('a')).expect("send");
        tx.send(key('b')).expect("send");
        let until = std::time::Instant::now() + WAIT;
        while reader.events().len() < 2 && std::time::Instant::now() < until {
            thread::sleep(SHORT);
        }
        assert_eq!(reader.events().len(), 2, "both read");
        reader.drain();
        tx.send(key('c')).expect("send");
        assert_eq!(reader.events().recv_timeout(WAIT).ok(), Some(key('c')));
    }

    #[test]
    fn a_pause_of_a_thread_that_ended_returns() {
        let (tx, reader) = reader();
        drop(tx);
        // The source closed: the thread ends and the events with it.
        assert!(reader.events().recv_timeout(WAIT).is_err());
        drop(reader.pause());
    }
}

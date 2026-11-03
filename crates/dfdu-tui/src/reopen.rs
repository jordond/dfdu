//! Opening a scan of another path on the host scanned while the browser runs ([`open_other`]).
//! A source that talks to the user (ssh asking for a password) gets the terminal while it is
//! built ([`Handover`]): the input thread is paused so no key is taken from it, and the
//! terminal is taken back after. When it cannot be, the run ends with the error and what was
//! opened is shut down.

use std::io;
use std::time::Duration;

use dfdu_session::startup::Opened;
use dfdu_tree::HostPath;

use crate::input::reader::{Paused, Reader};
use crate::setup;

/// How long a new scan that cannot be shown gets to stop.
const SHUTDOWN: Duration = Duration::from_millis(200);

/// Giving the terminal to something that talks to the user, and taking it back.
pub(crate) trait Handover {
    /// Gives the terminal back for a scan of `path`: no input read, raw mode and the
    /// alternate screen left.
    fn release(&mut self, path: &HostPath);
    /// Takes the terminal again, and reads input again.
    ///
    /// # Errors
    ///
    /// When the terminal cannot be put back in raw mode and the alternate screen.
    fn reclaim(&mut self) -> io::Result<()>;
}

/// The browser's own terminal and input thread.
pub(crate) struct Hand<'a> {
    terminal: &'a mut ratatui::DefaultTerminal,
    reader: &'a Reader,
    mouse: bool,
    paused: Option<Paused>,
}

impl<'a> Hand<'a> {
    pub(crate) fn new(
        terminal: &'a mut ratatui::DefaultTerminal,
        reader: &'a Reader,
        mouse: bool,
    ) -> Self {
        Hand {
            terminal,
            reader,
            mouse,
            paused: None,
        }
    }
}

impl Handover for Hand<'_> {
    fn release(&mut self, path: &HostPath) {
        self.paused = Some(self.reader.pause());
        setup::restore_terminal(self.mouse);
        eprintln!("dfdu: scanning {path}…");
    }

    fn reclaim(&mut self) -> io::Result<()> {
        let back = setup::resume_terminal(self.terminal, self.mouse);
        self.paused = None;
        back
    }
}

/// Opens a scan of `path` with `open`. With `suspend` the terminal is handed over while
/// `open` runs. Returns what it opened, or why it could not (for the status line).
///
/// # Errors
///
/// When the terminal cannot be taken back: what was opened is given to `shut`, and the
/// browser cannot go on.
pub(crate) fn open_other<N>(
    hand: &mut impl Handover,
    suspend: bool,
    path: &HostPath,
    open: impl FnOnce() -> io::Result<N>,
    shut: impl FnOnce(N),
) -> io::Result<Result<N, String>> {
    let failed = |e: io::Error| format!("cannot scan {path}: {e}");
    if !suspend {
        return Ok(open().map_err(failed));
    }
    hand.release(path);
    let opened = open();
    if let Err(e) = hand.reclaim() {
        if let Ok(n) = opened {
            shut(n);
        }
        return Err(io::Error::new(
            e.kind(),
            format!("cannot take the terminal back: {e}"),
        ));
    }
    Ok(opened.map_err(failed))
}

/// Stops the engine of a scan that will not be shown.
pub(crate) fn shut_down(opened: Opened) {
    match opened {
        Opened::Native(s) => s.engine.shutdown(SHUTDOWN),
        Opened::Imported(s) => s.engine.shutdown(SHUTDOWN),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records what was done with the terminal; `reclaim` fails when `broken`.
    #[derive(Default)]
    struct Fake {
        log: Vec<&'static str>,
        broken: bool,
    }

    impl Handover for Fake {
        fn release(&mut self, _: &HostPath) {
            self.log.push("release");
        }

        fn reclaim(&mut self) -> io::Result<()> {
            self.log.push("reclaim");
            if self.broken {
                Err(io::Error::other("no tty"))
            } else {
                Ok(())
            }
        }
    }

    fn path() -> HostPath {
        HostPath::from("/mnt/disk1")
    }

    #[test]
    fn a_source_that_does_not_talk_keeps_the_terminal() {
        let mut hand = Fake::default();
        let got = open_other(&mut hand, false, &path(), || Ok(7), |_| panic!("shut"));
        assert_eq!(got.ok(), Some(Ok(7)));
        assert!(hand.log.is_empty());
        let got = open_other::<u8>(
            &mut hand,
            false,
            &path(),
            || Err(io::Error::other("refused")),
            |_| panic!("shut"),
        );
        assert_eq!(
            got.ok(),
            Some(Err("cannot scan /mnt/disk1: refused".into()))
        );
    }

    #[test]
    fn a_source_that_talks_gets_the_terminal_and_gives_it_back() {
        let mut hand = Fake::default();
        let got = open_other(&mut hand, true, &path(), || Ok(7), |_| panic!("shut"));
        assert_eq!(got.ok(), Some(Ok(7)));
        assert_eq!(hand.log, ["release", "reclaim"]);

        // A failed connection is shown in the browser it was asked from.
        let mut hand = Fake::default();
        let got = open_other::<u8>(
            &mut hand,
            true,
            &path(),
            || Err(io::Error::other("no route")),
            |_| panic!("shut"),
        );
        assert_eq!(
            got.ok(),
            Some(Err("cannot scan /mnt/disk1: no route".into()))
        );
        assert_eq!(hand.log, ["release", "reclaim"]);
    }

    #[test]
    fn a_terminal_not_taken_back_ends_the_run_and_shuts_the_new_scan_down() {
        let mut hand = Fake {
            broken: true,
            ..Fake::default()
        };
        let mut shut = None;
        let got = open_other(&mut hand, true, &path(), || Ok(7), |n| shut = Some(n));
        let err = got.err().map(|e| e.to_string()).unwrap_or_default();
        assert!(err.contains("cannot take the terminal back"), "{err}");
        assert_eq!(shut, Some(7));
    }
}

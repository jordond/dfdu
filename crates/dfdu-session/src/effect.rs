//! What a key or a click leaves to be carried out after the state changed ([`Effect`]):
//! commands for the engine, reads of the model, and what only the front end can do (the
//! clipboard, a file manager). [`carry_out`] does the engine's part and returns the rest
//! ([`Output`]).

use std::path::PathBuf;

use dfdu_engine::{Command, Engine, ErrorList, errors};
use dfdu_scan::Backend;

use crate::dupes;
use crate::state::SessionState;

/// Most entries the error list shows.
pub const ERROR_LIST_MAX: usize = 10_000;

#[derive(Debug)]
pub enum Effect {
    Send(Command),
    /// Read the error list from the engine and show it.
    ShowErrors,
    /// Read the duplicate candidates and start comparing them (`dupes::begin`).
    FindDupes,
    /// Put the text on the clipboard.
    Copy(String),
    /// Show the path in the file manager, without waiting for it.
    Open {
        path: PathBuf,
        is_dir: bool,
    },
}

/// What [`carry_out`] leaves to the front end.
#[derive(Debug)]
pub enum Output {
    /// Show what could not be read.
    Errors(ErrorList),
    /// Put the text on the clipboard: the terminal's (OSC 52) or the system's.
    Copy(String),
    /// Show the path in the file manager, without waiting for it.
    Open { path: PathBuf, is_dir: bool },
}

/// Carries out `fx` in order: sends the commands to `engine`, reads the error list and starts a
/// comparison of duplicates from the candidates read under its read lock. Returns, in order,
/// what only the front end can do.
pub fn carry_out<B: Backend>(
    s: &mut SessionState,
    engine: &Engine<B>,
    fx: Vec<Effect>,
) -> Vec<Output> {
    let mut out = Vec::new();
    for fx in fx {
        match fx {
            Effect::Send(cmd) => engine.send(cmd),
            Effect::FindDupes => {
                let (root, cands) = engine.read(|m| match m.tree.roots().next() {
                    Some(r) => (
                        m.tree.path_buf(r),
                        dfdu_dupes::candidates(&m.tree, r, dupes::min_size()),
                    ),
                    None => (PathBuf::new(), Vec::new()),
                });
                dupes::begin(s, root, cands);
            }
            Effect::ShowErrors => {
                let list = engine.read(|m| match m.tree.roots().next() {
                    Some(root) => errors(&m.tree, root, ERROR_LIST_MAX),
                    None => ErrorList::default(),
                });
                out.push(Output::Errors(list));
            }
            Effect::Copy(text) => out.push(Output::Copy(text)),
            Effect::Open { path, is_dir } => out.push(Output::Open { path, is_dir }),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use dfdu_engine::EngineOptions;
    use dfdu_scan::PortableBackend;

    use super::*;

    #[test]
    fn the_engine_part_is_carried_out_and_the_rest_returned_in_order() {
        let engine = Engine::start(
            PortableBackend::default(),
            EngineOptions {
                workers: 1,
                ..Default::default()
            },
        );
        let mut s = SessionState::new(Instant::now());
        let fx = vec![
            Effect::Copy("/r/a".into()),
            Effect::ShowErrors,
            Effect::Open {
                path: "/r".into(),
                is_dir: true,
            },
        ];
        let out = carry_out(&mut s, &engine, fx);
        assert!(
            matches!(&out[..], [Output::Copy(t), Output::Errors(_), Output::Open { is_dir: true, .. }] if t == "/r/a"),
            "{out:?}"
        );
        // Without a scan there are no candidates; the comparison of none starts all the same.
        assert!(carry_out(&mut s, &engine, vec![Effect::FindDupes]).is_empty());
        assert!(s.dupes.run.is_some());
        engine.shutdown(Duration::from_millis(200));
    }
}

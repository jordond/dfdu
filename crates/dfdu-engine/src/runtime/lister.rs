//! The engine's way to list on the agent's host ([`Engine::lister`]).

use dfdu_scan::Backend;

use super::Engine;
use crate::remote::Lister;

impl<B: Backend> Engine<B> {
    /// Lists directories and mounts on the agent's host, off the caller's thread: `None` for
    /// an engine that does not scan through an agent, whose host is this one or none.
    #[must_use]
    pub fn lister(&self) -> Option<Lister> {
        self.link.clone().map(Lister::new)
    }
}

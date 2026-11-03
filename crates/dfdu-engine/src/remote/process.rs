//! The process carrying a link to an agent (the `ssh` running it), shared between the link and whoever started it.

use parking_lot::Mutex;
use std::process::Child;
use std::sync::Arc;

/// The process carrying a link, shared: the link kills it when dead, and whoever started it can
/// kill it too, such as to cancel while the agent has not said hello yet.
#[derive(Clone, Default)]
pub struct Process(Arc<Mutex<Option<Child>>>);

impl Process {
    /// Kills it, if it still runs.
    pub fn kill(&self) {
        if let Some(c) = self.0.lock().as_mut() {
            let _ = c.kill();
        }
    }

    /// Takes the process out, to wait for it; the handle kills nothing after.
    #[must_use]
    pub fn take(&self) -> Option<Child> {
        self.0.lock().take()
    }
}

impl From<Child> for Process {
    fn from(c: Child) -> Self {
        Process(Arc::new(Mutex::new(Some(c))))
    }
}

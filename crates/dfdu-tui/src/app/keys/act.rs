//! The actions of [`run`](super::run) that take more than a line.

use dfdu_engine::{Command, SizeMode};

use crate::app::{App, Effect};
use crate::model::snapshot::Mark;
use crate::state::order::{SortKey, SortSpec};

/// A sort key: the key's default direction, or the other direction if it is already in use.
pub(super) fn sort_by(app: &mut App, key: SortKey) {
    app.sort = if app.sort.key == key {
        SortSpec {
            key,
            desc: !app.sort.desc,
        }
    } else {
        SortSpec::for_key(key)
    };
    app.request_sort();
}

/// Asks the engine to scan into the selected mount the scan stayed out of.
pub(super) fn enter_mount(app: &mut App) -> Option<Effect> {
    match app.selected_entry() {
        Some(e) if matches!(e.mark, Mark::OtherFs | Mark::Network) => {
            let name = String::from_utf8_lossy(&e.name).into_owned();
            let node = e.node;
            app.status = Some(format!("scanning {name}/ on its own filesystem…"));
            Some(Effect::Send(Command::Enter { node }))
        }
        _ => {
            app.status = Some("m scans a mount the scan stayed out of; select one first".into());
            None
        }
    }
}

/// Switches to `mode`, or back to disk usage if `mode` is shown already.
pub(super) fn toggle_size(app: &mut App, mode: SizeMode) {
    app.size_mode = if app.size_mode == mode {
        SizeMode::Disk
    } else {
        mode
    };
    app.request_sort();
}

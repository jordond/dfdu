//! What grew since the last recorded scan of the root (`dfdu_store::history`): a note on list
//! rows whose size changed noticeably, and a line in the details pane. `growth` is the last
//! scan's sizes, once loaded ([`crate::history_link::Polled::growth`]).

use dfdu_engine::SizeMode;
use dfdu_store::history::Growth;
use dfdu_tree::HostPath;

use crate::snapshot::Mark;

/// Changes smaller than this are not noted on list rows.
const NOTICEABLE: u64 = 1 << 20;

/// The change in the size shown in `size_mode` of `path` (now `size`) since the last recorded
/// scan, with its size then and when that scan finished. `None` when the history has no size
/// for it, the size mode is not recorded, or the size is still changing.
#[must_use]
pub fn since(
    growth: Option<&Growth>,
    size_mode: SizeMode,
    path: &HostPath,
    size: u64,
    mark: Mark,
) -> Option<(i128, u64, i64)> {
    if matches!(
        mark,
        Mark::Scanning | Mark::Rescanning | Mark::Stalled | Mark::Incomplete | Mark::Deleting
    ) {
        return None;
    }
    // The history is of this machine's scans only.
    let g = growth?;
    let then = g.get(&path.to_local())?;
    let then = match size_mode {
        SizeMode::Disk => then.disk,
        SizeMode::Apparent => then.apparent,
        SizeMode::Referenced => return None,
    };
    Some((i128::from(size) - i128::from(then), then, g.at))
}

/// "+12.4 GiB since 2026-10-18".
#[must_use]
pub fn describe(delta: i128, at: i64) -> String {
    format!("{} since {}", short(delta), dfdu_tree::day(at))
}

/// "+12.4 GiB".
fn short(delta: i128) -> String {
    let sign = if delta < 0 { "-" } else { "+" };
    let abs = u64::try_from(delta.unsigned_abs()).unwrap_or(u64::MAX);
    format!("{sign}{}", dfdu_engine::human(abs))
}

/// A noticeable change of a list row's size since the last recorded scan.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct RowNote {
    pub delta: i128,
    /// When the last recorded scan finished.
    pub at: i64,
}

impl RowNote {
    #[must_use]
    pub fn grew(self) -> bool {
        self.delta > 0
    }

    /// "+3.4 GiB", for the column of changes.
    #[must_use]
    pub fn short(self) -> String {
        short(self.delta)
    }

    /// "+3.4 GiB since 2026-10-18", for the selection line.
    #[must_use]
    pub fn long(self) -> String {
        describe(self.delta, self.at)
    }
}

/// The note for a list row: changes of at least 1 MiB and 1% of the size.
#[must_use]
pub fn row_note(
    growth: Option<&Growth>,
    size_mode: SizeMode,
    path: &HostPath,
    size: u64,
    mark: Mark,
) -> Option<RowNote> {
    let (delta, then, at) = since(growth, size_mode, path, size, mark)?;
    let abs = delta.unsigned_abs();
    if abs < u128::from(NOTICEABLE) || abs * 100 < u128::from(then.max(size)) {
        return None;
    }
    Some(RowNote { delta, at })
}

/// The header of the column of changes, with the day of the last recorded scan: "Δ 10-18".
#[must_use]
pub fn column_title(growth: Option<&Growth>) -> String {
    match growth.map(|g| dfdu_tree::day(g.at)) {
        Some(day) => format!("Δ {}", day.get(5..).unwrap_or(&day)),
        None => "Δ".into(),
    }
}

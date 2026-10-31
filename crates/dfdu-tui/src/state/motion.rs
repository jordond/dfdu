//! Motion: bars that ease to new lengths, rank markers after automatic re-sorts, and deleted
//! rows kept in place as placeholders. Frames are scheduled only while something moves.
//!
//! Every animation ends: a bar stops easing within a sixteenth of a cell of its target, rank
//! markers go after [`MARKERS`], the flash of moved rows after [`FLASH`] and the fade of a
//! deleted row after [`FADE`]. The colour effects ([`effects`]) run only at `TrueColor`, on a
//! background that is known or that the theme's RGB text colour was chosen for
//! ([`effects::rgb_effects`]); otherwise moved rows flash reversed (except the selected one,
//! which keeps its highlight) and deleted rows are only struck through.

mod effects;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use dfdu_engine::SizeMode;
use dfdu_tree::NodeRef;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Span;

use crate::app::App;
use crate::format::ratio;
use crate::model::snapshot::{EntryData, Mark, Snapshot};

/// Time between animation frames.
const FRAME: Duration = Duration::from_millis(33);
/// Time constant of the bar easing: a bar covers 1 - 1/e of the way to its target in this time.
const TAU: f64 = 0.120;
/// The longest step one frame eases by, so a frame drawn late does not make the bars jump. A
/// frame that comes later than this after the last one stretches the easing by the difference.
pub const MAX_STEP: Duration = Duration::from_millis(100);
/// A bar this close to its target (a sixteenth of a cell of the widest bar, 24 cells) is drawn
/// at the target and stops easing.
const SNAP: f64 = 1.0 / (16.0 * 24.0);
/// How long rank markers stay after an automatic re-sort.
const MARKERS: Duration = Duration::from_secs(2);
/// The fewest places the selected row moves by to get a rank marker.
const RANK_MIN: u32 = 3;
/// How long moved rows flash.
const FLASH: Duration = Duration::from_millis(400);
/// Shortest time between the starts of two flashes.
const FLASH_GAP: Duration = Duration::from_secs(1);
/// How long a deleted row takes to fade.
const FADE: Duration = Duration::from_millis(250);

/// What the shown bar lengths belong to: the directory, the size mode and the list area. When
/// any of them changes, bars start at their targets.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Scope {
    dir: Option<NodeRef>,
    mode: SizeMode,
    area: Rect,
}

#[derive(Clone, Debug, Default)]
pub struct Motion {
    on: bool,
    scope: Option<Scope>,
    /// Shown fraction of the bar of each row on screen.
    bars: HashMap<NodeRef, f64>,
    /// The previous frame's `bars`, kept for its allocation.
    spare: HashMap<NodeRef, f64>,
    /// Entry indices of the rows on screen in the last frame, top to bottom.
    shown: Vec<usize>,
    /// Rank change per entry index in the last automatic re-sort, until the next frame turns it
    /// into markers for the rows on screen.
    moves: Option<Vec<i32>>,
    /// Rows on screen that the last automatic re-sort moved: up for positive counts.
    markers: HashMap<NodeRef, i32>,
    marked_at: Option<Instant>,
    flash_at: Option<Instant>,
    last_flash: Option<Instant>,
    /// Deleted entries kept as placeholders, with the time they were deleted.
    ghosts: HashMap<NodeRef, Instant>,
    /// The last snapshot did not keep the entry indices of the one before: `before` holds the
    /// nodes of the one before, to compare the rows before a re-sort with the rows after it.
    /// Valid until the next frame.
    replaced: bool,
    before: Vec<NodeRef>,
    /// Time of the last frame, and whether a bar was still easing then.
    last: Option<Instant>,
    easing: bool,
    next: Option<Instant>,
}

impl Motion {
    /// Called with each new snapshot `new` of the directory shown in `old`, before it replaces
    /// `old`: adds a ghost entry for each entry that was deleted, and again for each ghost
    /// until the next re-sort. A ghost keeps the deleted entry's node, so it keeps its row.
    pub fn carry_ghosts(&mut self, old: &Snapshot, new: &mut Snapshot, now: Instant) {
        if !self.on {
            return;
        }
        self.replaced = false;
        self.before.clear();
        // Entries a type filter leaves out were not deleted.
        if old.dir_path != new.dir_path || old.types != new.types {
            self.ghosts.clear();
            return;
        }
        // A small delete can finish between two snapshots, so the entry is never seen marked
        // as deleting: while a delete runs or just finished, every entry that went counts.
        let plan = |s: &Snapshot| s.last_delete.as_ref().map(|d| d.plan);
        let deleting = old.deleting.is_some() || new.deleting.is_some() || plan(old) != plan(new);
        if deleting
            || !self.ghosts.is_empty()
            || old.entries.iter().any(|e| e.mark == Mark::Deleting)
        {
            self.ghosts = self.add_ghosts(old, new, deleting, now);
        }
        let kept = old.entries.len() <= new.entries.len()
            && old
                .entries
                .iter()
                .zip(&new.entries)
                .all(|(a, b)| a.node == b.node);
        if !kept {
            self.replaced = true;
            self.before.extend(old.entries.iter().map(|e| e.node));
        }
    }

    /// Appends the ghosts to `new` and returns them with their times of deletion. In tree mode
    /// a ghost stays under the row it was in, found again in `new`; one whose row is gone too
    /// (a collapsed directory) is dropped.
    fn add_ghosts(
        &self,
        old: &Snapshot,
        new: &mut Snapshot,
        deleting: bool,
        now: Instant,
    ) -> HashMap<NodeRef, Instant> {
        let present: HashSet<NodeRef> = new.entries.iter().map(|e| e.node).collect();
        let gone: Vec<(usize, &EntryData)> = old
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| !present.contains(&e.node))
            .filter(|(_, e)| {
                if e.ghost {
                    self.ghosts.contains_key(&e.node)
                } else {
                    !e.pseudo && (deleting || e.mark == Mark::Deleting)
                }
            })
            .collect();
        // A rescan replaces nodes: an entry with the same name and identity was not deleted.
        let names: HashSet<(&[u8], (u64, u64))> = gone
            .iter()
            .map(|&(_, e)| e)
            .filter(|e| !e.ghost)
            .map(|e| (&e.name[..], e.dev_ino))
            .collect();
        let swapped: HashSet<(&[u8], (u64, u64))> = if names.is_empty() {
            HashSet::new()
        } else {
            new.entries
                .iter()
                .map(|e| (&e.name[..], e.dev_ino))
                .filter(|k| names.contains(k))
                .collect()
        };
        // The rows of the old entries in the new ones, by node, for ghosts below the top.
        let nested = gone.iter().any(|&(_, e)| e.parent != u32::MAX);
        let row_of: HashMap<NodeRef, usize> = if nested {
            new.entries
                .iter()
                .enumerate()
                .map(|(i, e)| (e.node, i))
                .collect()
        } else {
            HashMap::new()
        };
        let mut moved: HashMap<usize, usize> = HashMap::new();
        let mut ghosts = HashMap::new();
        let mut added = Vec::new();
        for (i, e) in gone {
            let deleted_at = if e.ghost {
                self.ghosts[&e.node]
            } else if !swapped.contains(&(&e.name[..], e.dev_ino)) {
                now
            } else {
                continue;
            };
            let p = e.parent as usize;
            let parent = if e.parent == u32::MAX {
                Some(u32::MAX)
            } else {
                (moved.get(&p).copied())
                    .or_else(|| row_of.get(&old.entries.get(p)?.node).copied())
                    .and_then(|r| u32::try_from(r).ok())
            };
            let Some(parent) = parent else {
                continue;
            };
            moved.insert(i, new.entries.len() + added.len());
            ghosts.insert(e.node, deleted_at);
            added.push(EntryData {
                ghost: true,
                pseudo: true,
                is_dir: false,
                mark: Mark::None,
                parent,
                ..e.clone()
            });
        }
        new.entries.extend(added);
        ghosts
    }

    /// Called after a re-sort within one directory (`auto` for the automatic one) with the
    /// rows before and after, as indices into the previous and the current `entries`. Every
    /// re-sort drops the ghosts and the rank markers; an automatic one leaves the rank changes
    /// for the next frame to show.
    pub fn resorted(
        &mut self,
        old_rows: &[usize],
        new_rows: &[usize],
        entries: &[EntryData],
        auto: bool,
    ) {
        if !self.on {
            return;
        }
        self.ghosts.clear();
        self.markers.clear();
        self.marked_at = None;
        self.flash_at = None;
        self.moves = auto.then(|| {
            if !self.replaced {
                return moves(old_rows, new_rows, entries.len());
            }
            let at: HashMap<NodeRef, usize> = entries
                .iter()
                .enumerate()
                .map(|(i, e)| (e.node, i))
                .collect();
            let old_rows: Vec<usize> = old_rows
                .iter()
                .filter_map(|&i| at.get(self.before.get(i)?).copied())
                .collect();
            moves(&old_rows, new_rows, entries.len())
        });
        self.replaced = false;
        self.before.clear();
    }

    /// Whether deleted entries are kept as placeholders, until the next re-sort.
    pub fn has_ghosts(&self) -> bool {
        !self.ghosts.is_empty()
    }

    /// When the next animation frame is due; `None` while nothing moves.
    pub fn next_frame_at(&self) -> Option<Instant> {
        self.next
    }
}

/// How far each entry moved between `old_rows` and `new_rows`, up for positive counts, by entry
/// index. Positions count only the rows in both lists, so rows that came or went do not move
/// the others.
fn moves(old_rows: &[usize], new_rows: &[usize], len: usize) -> Vec<i32> {
    const ABSENT: u32 = u32::MAX;
    const IN_NEW: u32 = u32::MAX - 1;
    let mut rank = vec![ABSENT; len];
    for &i in new_rows {
        rank[i] = IN_NEW;
    }
    let mut r = 0;
    for &i in old_rows {
        if rank.get(i) == Some(&IN_NEW) {
            rank[i] = r;
            r += 1;
        }
    }
    let mut moves = vec![0; len];
    let mut r = 0;
    for &i in new_rows {
        if rank[i] < IN_NEW {
            moves[i] = i32::try_from(rank[i]).unwrap_or(i32::MAX).saturating_sub(r);
            r += 1;
        }
    }
    moves
}

/// Turns the animations on. [`App::new`] starts with them off, so frames drawn in tests do not
/// depend on timing; the run loop calls this once before its first frame.
pub fn start(app: &mut App) {
    app.motion.on = true;
}

/// Advances the animations of the rows on screen (`visible`: entry indices) to `now`, and
/// decides when the next frame is due.
pub fn frame(app: &mut App, now: Instant, visible: &[usize]) {
    let m = &mut app.motion;
    if !m.on {
        return;
    }
    // After a pause, bars that just got a new target start with one frame's step.
    let dt = match m.last {
        Some(t) if m.easing => now.saturating_duration_since(t).min(MAX_STEP),
        _ => FRAME,
    };
    m.last = Some(now);
    let scope = Scope {
        dir: app.snap.dir_node,
        mode: app.size_mode,
        area: app.areas.list,
    };
    if m.scope != Some(scope) {
        if m.scope.is_some_and(|s| s.dir != scope.dir) {
            m.markers.clear();
            m.marked_at = None;
            m.flash_at = None;
            m.moves = None;
        }
        m.bars.clear();
        m.scope = Some(scope);
    }

    let entries = &app.snap.entries;
    let largest = app.list.rows_largest.max(1);
    let step = 1.0 - (-dt.as_secs_f64() / TAU).exp();
    let mut easing = false;
    let mut bars = std::mem::take(&mut m.spare);
    bars.clear();
    for &i in visible {
        let e = &entries[i];
        let target = ratio(e.size, largest);
        let shown = match m.bars.get(&e.node) {
            Some(&s) => {
                let s = s + (target - s) * step;
                if (target - s).abs() < SNAP {
                    target
                } else {
                    easing = true;
                    s
                }
            }
            None => target,
        };
        bars.insert(e.node, shown);
    }
    m.spare = std::mem::replace(&mut m.bars, bars);
    m.shown.clear();
    m.shown.extend_from_slice(visible);

    if let Some(moves) = m.moves.take() {
        m.markers.clear();
        for &i in visible {
            if let Some(&d) = moves.get(i)
                && d != 0
            {
                m.markers.insert(entries[i].node, d);
            }
        }
        if !m.markers.is_empty() {
            m.marked_at = Some(now);
            if m.last_flash
                .is_none_or(|t| now.saturating_duration_since(t) >= FLASH_GAP)
            {
                m.flash_at = Some(now);
                m.last_flash = Some(now);
            }
        }
    }
    if m.marked_at.is_some_and(|t| now >= t + MARKERS) {
        m.markers.clear();
        m.marked_at = None;
    }
    if m.flash_at.is_some_and(|t| now >= t + FLASH) {
        m.flash_at = None;
    }

    // Colour effects change every frame; the reversed flash only when it starts and ends.
    let rgb = effects::rgb_effects(&app.theme);
    let fading = rgb && m.ghosts.values().any(|&t| now < t + FADE);
    let mut next = (easing || fading || (rgb && m.flash_at.is_some())).then(|| now + FRAME);
    let ends = [
        m.flash_at.filter(|_| !rgb).map(|t| t + FLASH),
        m.marked_at.map(|t| t + MARKERS),
    ];
    for t in ends.into_iter().flatten() {
        next = Some(next.map_or(t, |n| n.min(t)));
    }
    m.next = next;
    m.easing = easing;
    m.replaced = false;
    m.before.clear();
}

/// The fraction of the bar shown for `e`, whose size is `frac` of the largest row.
pub fn bar(app: &App, e: &EntryData, frac: f64) -> f64 {
    app.motion.bars.get(&e.node).copied().unwrap_or(frac)
}

/// The rank marker after the name of `e`, if it is the selected row and moved by at least
/// [`RANK_MIN`] places in the last automatic re-sort. Other rows that moved only flash: a marker
/// on each of them would cover the list in arrows while a scan runs.
pub fn rank(app: &App, e: &EntryData) -> Option<Span<'static>> {
    let &d = app.motion.markers.get(&e.node)?;
    let selected = app.selected.as_ref().and_then(|s| s.node) == Some(e.node);
    if !selected || d.unsigned_abs() < RANK_MIN {
        return None;
    }
    let arrow = if d > 0 { '↑' } else { '↓' };
    Some(Span::styled(
        format!("  {arrow}{}", d.unsigned_abs().min(99)),
        app.theme.fg_dim,
    ))
}

/// Effects applied to the finished frame: the flash of moved rows and the fade of deleted ones,
/// on the rows where [`frame`] last found them, except where an overlay covers them.
pub fn post_process(app: &App, buf: &mut Buffer, now: Instant) {
    let m = &app.motion;
    let rgb = effects::rgb_effects(&app.theme);
    let flash = m.flash_at.map(|t| now.saturating_duration_since(t));
    if !m.on || (flash.is_none() && (!rgb || m.ghosts.is_empty())) {
        return;
    }
    // Reversing the selected row would take its highlight off.
    let keep = app.selected_entry().filter(|_| !rgb).map(|e| e.node);
    let list = app.areas.list;
    let covers = [app.areas.overlay, app.areas.debug];
    for (y, &i) in (list.top()..list.bottom()).zip(&m.shown) {
        let Some(e) = app.snap.entries.get(i) else {
            continue;
        };
        let flashing = flash.filter(|_| m.markers.contains_key(&e.node) && keep != Some(e.node));
        let fading = m
            .ghosts
            .get(&e.node)
            .filter(|_| rgb && e.ghost)
            .map(|&t| now.saturating_duration_since(t).min(FADE));
        if flashing.is_none() && fading.is_none() {
            continue;
        }
        let row = Rect::new(list.x, y, list.width, 1).intersection(buf.area);
        for part in uncovered(row, &covers) {
            if let Some(d) = flashing {
                effects::flash(&app.theme, buf, part, d);
            }
            if let Some(d) = fading {
                effects::fade(&app.theme, buf, part, d);
            }
        }
    }
}

/// The parts of `row` (one line high) that none of `covers` covers.
fn uncovered(row: Rect, covers: &[Option<Rect>]) -> Vec<Rect> {
    let mut parts = vec![row];
    for &c in covers.iter().flatten() {
        parts = parts.into_iter().flat_map(|p| beside(p, c)).collect();
    }
    parts
}

/// The parts of `row` (one line high) left and right of `cover`.
fn beside(row: Rect, cover: Rect) -> impl Iterator<Item = Rect> {
    let parts = if cover.intersects(row) {
        [
            Rect {
                width: cover.x.saturating_sub(row.x).min(row.width),
                ..row
            },
            Rect {
                x: cover.right(),
                width: row.right().saturating_sub(cover.right()),
                ..row
            },
        ]
    } else {
        [row, Rect::default()]
    };
    parts.into_iter().filter(|r| !r.is_empty())
}

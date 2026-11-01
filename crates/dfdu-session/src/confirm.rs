//! The delete confirmation, for the marked entries or else the selected one: the whole gate a
//! delete passes before the engine gets it. Opening it asks the engine what the delete frees
//! ([`Command::Estimate`]); confirming sends one [`Command::Delete`] of every target that is not
//! inside another. The method is a move to the Trash, staging (renamed aside at once, put back
//! for a minute, then deleted) or a permanent delete. A permanent delete, and a move to the
//! Trash of [`LARGE`] or more or of a directory still being scanned, needs [`WORD`] typed. A
//! read-only browser opens none.
//!
//! The open dialog is [`SessionState::confirm`]; a front end draws it and turns its input into
//! [`Intent`]s, the typed word's as [`TextOp`]s.

use std::cmp::Reverse;
use std::collections::HashSet;

use dfdu_engine::{Command, Mode, rel_text};
use dfdu_tree::NodeRef;

use crate::effect::Effect;
use crate::line::{LineInput, TextOp};
use crate::marks;
use crate::snapshot::{EntryData, Mark};
use crate::state::{NO_FOLDERS, READ_ONLY, SessionState};
use crate::view::Then;

/// The word a typed confirmation asks for.
pub const WORD: &str = "delete";
/// Longest text the confirmation input takes.
const TYPED_MAX: usize = 16;
/// A move to the Trash of this much or more also needs the word typed.
pub const LARGE: u64 = 50 << 30;

/// One entry the dialog lists.
#[derive(Clone, Debug)]
pub struct Item {
    pub node: NodeRef,
    /// The name, or the path relative to the current directory (the full path when it is not
    /// below it).
    pub name: String,
    pub size: u64,
    pub items: u64,
    /// Part of `size` that data outside the scan also uses.
    pub shared_out: u64,
    pub is_dir: bool,
    /// A directory still being scanned: `size` is a lower bound.
    pub scanning: bool,
    /// Below another target: deleted with it and not counted again.
    pub inside: bool,
}

impl Item {
    fn of(e: &EntryData) -> Self {
        Item {
            node: e.node,
            name: String::from_utf8_lossy(&e.name).into_owned(),
            size: e.size,
            items: e.items,
            shared_out: e.shared_out,
            is_dir: e.is_dir,
            scanning: matches!(e.mark, Mark::Scanning | Mark::Stalled | Mark::Rescanning),
            inside: false,
        }
    }

    /// The name, with `/` after a directory.
    pub fn label(&self) -> String {
        if self.is_dir {
            format!("{}/", self.name)
        } else {
            self.name.clone()
        }
    }
}

/// The open dialog.
#[derive(Clone, Debug)]
pub struct Confirm {
    /// The targets sent to the engine, largest first, then those inside them.
    targets: Vec<Item>,
    /// Size, entries and size used outside the scan of the targets sent.
    total: u64,
    items: u64,
    shared_out: u64,
    mode: Mode,
    typed: LineInput,
    /// The word must be typed to confirm.
    typing: bool,
    /// The id of the engine's estimate of what the delete frees ([`Command::Estimate`]).
    estimate: u64,
}

/// What a front end's input asks of the open dialog.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Intent {
    /// Switches between a permanent delete and the Trash (from staging too).
    SwitchPermanent,
    /// Switches between staging and the Trash (from a permanent delete too).
    SwitchStage,
    /// Confirms: deletes when no word is asked for or the word is typed, and otherwise closes
    /// the dialog saying why nothing was deleted.
    Confirm,
    /// Closes the dialog without deleting.
    Cancel,
    /// Edits the typed word; [`TextOp::Submit`] confirms and [`TextOp::Cancel`] cancels.
    Text(TextOp),
}

impl Confirm {
    /// The dialog for `targets`, deleting by `mode`.
    pub fn new(mut targets: Vec<Item>, mode: Mode) -> Self {
        targets.sort_by_key(|t| (t.inside, Reverse(t.size)));
        let sent = targets.iter().filter(|t| !t.inside);
        let (total, items, shared_out) = sent.fold((0, 0, 0), |(n, i, s), t| {
            (n + t.size, i + t.items, s + t.shared_out)
        });
        let mut c = Confirm {
            targets,
            total,
            items,
            shared_out,
            mode,
            typed: LineInput::default(),
            typing: false,
            estimate: 0,
        };
        c.set_mode(mode);
        c
    }

    fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
        self.typing = mode == Mode::Permanent || self.total >= LARGE || self.scanning();
        self.typed.clear();
    }

    /// The targets, largest first, then those inside another.
    pub fn targets(&self) -> &[Item] {
        &self.targets
    }

    /// The targets sent to the engine: those not inside another.
    pub fn sent(&self) -> impl Iterator<Item = &Item> {
        self.targets.iter().filter(|t| !t.inside)
    }

    /// The size of the targets sent.
    pub fn total(&self) -> u64 {
        self.total
    }

    /// The entries of the targets sent.
    pub fn items(&self) -> u64 {
        self.items
    }

    /// The part of [`Confirm::total`] that data outside the scan also uses.
    pub fn shared_out(&self) -> u64 {
        self.shared_out
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// The word typed so far.
    pub fn typed(&self) -> &LineInput {
        &self.typed
    }

    /// The word must be typed to confirm.
    pub fn typing(&self) -> bool {
        self.typing
    }

    /// The id of the engine's estimate of what the delete frees.
    pub fn estimate(&self) -> u64 {
        self.estimate
    }

    /// A target sent is still being scanned: [`Confirm::total`] is a lower bound.
    pub fn scanning(&self) -> bool {
        self.sent().any(|t| t.scanning)
    }
}

/// Asks to delete the marked entries, the outermost of them, or without marks the selected
/// entry, and asks the engine what that would free. The "shared between entries" row and
/// deleted rows cannot be deleted.
pub fn open(s: &mut SessionState, mode: Mode) -> Vec<Effect> {
    let targets: Vec<Item> = if s.marks.is_empty() {
        let e = s.selected_entry().filter(|e| !e.pseudo);
        e.map(Item::of).into_iter().collect()
    } else {
        marked(s, |_| true)
    };
    ask(s, mode, targets)
}

/// Asks to delete the marked entries among `only` (a review view's): the other marks stay.
pub fn open_only(s: &mut SessionState, mode: Mode, only: &HashSet<NodeRef>) -> Vec<Effect> {
    let targets = marked(s, |n| only.contains(&n));
    ask(s, mode, targets)
}

/// The marked entries whose node `keep` lets in, named below the current directory.
/// A mark inside another counts as inside only while that other one is kept too: left out,
/// it is deleted on its own.
fn marked(s: &SessionState, keep: impl Fn(NodeRef) -> bool) -> Vec<Item> {
    let dir = &s.snap.dir_path;
    let kept: Vec<&marks::Item> = (s.marks.items().iter())
        .filter(|m| m.target.node.is_some_and(&keep))
        .collect();
    let all = kept.len() == s.marks.items().len();
    let below_a_kept = |m: &marks::Item| {
        kept.iter().any(|o| {
            o.target.node != m.target.node
                && m.target
                    .path
                    .rel(&o.target.path)
                    .is_some_and(|r| !r.is_empty())
        })
    };
    kept.iter()
        .filter_map(|m| {
            let path = &m.target.path;
            let shown = match rel_text(path, dir) {
                Some(rel) if !rel.is_empty() => rel,
                _ => path.to_string(),
            };
            Some(Item {
                node: m.target.node?,
                name: shown,
                size: m.size,
                items: m.items,
                shared_out: m.shared_out,
                is_dir: m.is_dir,
                scanning: m.scanning,
                inside: m.inside && (all || below_a_kept(m)),
            })
        })
        .collect()
}

/// Opens the dialog for `targets`, and asks the engine what deleting them would free. A
/// read-only browser says so instead, and a directory alone is refused while the type filter
/// shows only some of its files.
fn ask(s: &mut SessionState, mode: Mode, targets: Vec<Item>) -> Vec<Effect> {
    if s.read_only {
        s.status = Some(READ_ONLY.into());
        return Vec::new();
    }
    if targets.is_empty() {
        return Vec::new();
    }
    if s.marks.is_empty() && targets[0].is_dir && s.types_active() {
        s.status = Some(NO_FOLDERS.into());
        return Vec::new();
    }
    let mut c = Confirm::new(targets, mode);
    s.estimates += 1;
    c.estimate = s.estimates;
    let items = c.sent().map(|t| t.node).collect();
    s.confirm = Some(c);
    vec![Effect::Send(Command::Estimate {
        id: s.estimates,
        items,
    })]
}

/// Carries out `intent` on the open dialog. Confirming sends one delete of every target that
/// is not inside another; confirming and cancelling close the dialog.
pub fn act(s: &mut SessionState, intent: Intent) -> Then {
    let Some(c) = s.confirm.as_mut() else {
        return Then::nothing();
    };
    let confirmed = match intent {
        Intent::Cancel | Intent::Text(TextOp::Cancel) => false,
        Intent::SwitchPermanent => {
            let other = match c.mode {
                Mode::Trash | Mode::Stage => Mode::Permanent,
                Mode::Permanent => Mode::Trash,
            };
            c.set_mode(other);
            return Then::nothing();
        }
        Intent::SwitchStage => {
            let other = match c.mode {
                Mode::Stage => Mode::Trash,
                _ => Mode::Stage,
            };
            c.set_mode(other);
            return Then::nothing();
        }
        Intent::Confirm | Intent::Text(TextOp::Submit) => {
            let typed = c.typed.text() == WORD;
            if c.typing && !typed {
                s.status = Some("not deleted: type delete to confirm".into());
            }
            !c.typing || typed
        }
        Intent::Text(TextOp::Insert(_)) if c.typed.text().chars().count() >= TYPED_MAX => {
            return Then::nothing();
        }
        Intent::Text(op) => {
            // Submit and Cancel are taken above; the rest only edit.
            if c.typing {
                let _ = c.typed.apply(op);
            }
            return Then::nothing();
        }
    };
    let mut fx = Vec::new();
    if confirmed {
        let mode = c.mode;
        fx.push(Effect::Send(Command::Delete {
            items: c.sent().map(|t| t.node).collect(),
            mode,
        }));
        s.status = Some(match mode {
            Mode::Trash => "moving to the Trash…".into(),
            Mode::Stage => "staging…".into(),
            Mode::Permanent => "deleting…".into(),
        });
    }
    s.confirm = None;
    Then::effects(fx)
}

#[cfg(test)]
mod tests;

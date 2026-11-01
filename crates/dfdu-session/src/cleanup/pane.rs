//! The native command pane: an item's own cleanup command, its dry run first when it has one,
//! then [`Intent::Run`] runs it; [`Intent::OtherForm`] picks a further-reaching form, which runs
//! only when asked twice. A read-only browser runs only the dry runs. The pane is
//! [`Cleanup::pane`](super::Cleanup::pane) while it is open; a front end draws it with the
//! output read for it ([`output`]).

use dfdu_engine::Command;
use dfdu_tree::{NodeRef, RuleIx};

use super::{Item, NativeView, selected};
use crate::effect::Effect;
use crate::state::{READ_ONLY, SessionState};

/// The native command pane of one item.
#[derive(Clone, Debug)]
pub struct Pane {
    pub node: NodeRef,
    pub rule: RuleIx,
    /// The further-reaching form chosen, an index into the rule's variants; `None` for the
    /// command itself.
    pub variant: Option<usize>,
    /// The command whose output is shown; 0 before anything ran (no dry run to show).
    pub id: u64,
    pub dry_run: bool,
    /// A variant's real run was asked for once; asking again runs it.
    pub armed: bool,
}

/// What a front end's keys ask of the open pane.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Intent {
    /// Closes the pane, unless its command runs.
    Close,
    /// Picks the next form of the command, unless one runs; its dry run starts when it has one.
    OtherForm,
    /// Runs the command for real, once nothing ran yet (no dry run to show) or after a dry run
    /// that worked. A further-reaching form runs on the second ask.
    Run,
}

/// The output of the pane's command, as the last read found it.
pub fn output<'a>(s: &'a SessionState, p: &Pane) -> Option<&'a NativeView> {
    (s.extras.cleanup.native.as_ref()).filter(|n| n.id == p.id)
}

/// Opens the selected item's native command, starting with its dry run if it has one. Without
/// one, the status line says so.
pub fn open(s: &mut SessionState) -> Vec<Effect> {
    let Some(it) = selected(s).cloned() else {
        return Vec::new();
    };
    let Some(rules) = s.snap.rules.clone() else {
        return Vec::new();
    };
    let Some(def) = rules.rule(it.class.rule).action.native() else {
        s.status = Some(if s.read_only {
            "no native command for this item".into()
        } else {
            "no native command for this item; d or D deletes it".into()
        });
        return Vec::new();
    };
    if def.dry_run.is_empty() {
        s.cleanup.pane = Some(Pane {
            node: it.node,
            rule: it.class.rule,
            variant: None,
            id: 0,
            dry_run: true,
            armed: false,
        });
        return Vec::new();
    }
    start(s, &it, None, true)
}

fn start(s: &mut SessionState, it: &Item, variant: Option<usize>, dry_run: bool) -> Vec<Effect> {
    s.cleanup.commands += 1;
    // Ids from the view are above any the dialog uses.
    let id = (1 << 32) + s.cleanup.commands;
    s.cleanup.pane = Some(Pane {
        node: it.node,
        rule: it.class.rule,
        variant,
        id,
        dry_run,
        armed: false,
    });
    vec![Effect::Send(Command::RunNative {
        id,
        node: it.node,
        variant,
        dry_run,
    })]
}

/// Carries out `intent` on the open pane.
pub fn act(s: &mut SessionState, intent: Intent) -> Vec<Effect> {
    let Some(p) = s.cleanup.pane.clone() else {
        return Vec::new();
    };
    let Some(rules) = s.snap.rules.clone() else {
        return Vec::new();
    };
    let Some(def) = rules.rule(p.rule).action.native() else {
        return Vec::new();
    };
    // Until its result has arrived, the shown command counts as running.
    let done = output(s, &p).and_then(|n| n.done.clone());
    let running = p.id != 0 && done.is_none();
    // The real run only after nothing ran yet (no dry run to show) or a dry run that worked.
    let may_run = p.id == 0 || (p.dry_run && done == Some(Ok(())));
    if s.read_only && may_run && intent == Intent::Run {
        s.status = Some(READ_ONLY.into());
        return Vec::new();
    }
    let it = (s.extras.cleanup.items.iter())
        .find(|i| i.node == p.node)
        .cloned();
    match intent {
        Intent::Close if !running => s.cleanup.pane = None,
        Intent::OtherForm if !running => {
            let next = match p.variant {
                None if !def.variants.is_empty() => Some(0),
                Some(v) if v + 1 < def.variants.len() => Some(v + 1),
                _ => None,
            };
            let dry = match next {
                None => !def.dry_run.is_empty(),
                Some(v) => !def.variants[v].dry_run.is_empty(),
            };
            if let Some(it) = it {
                if dry {
                    return start(s, &it, next, true);
                }
                s.cleanup.pane = Some(Pane {
                    variant: next,
                    id: 0,
                    dry_run: true,
                    armed: false,
                    ..p
                });
            }
        }
        Intent::Run if may_run => {
            let Some(it) = it else {
                return Vec::new();
            };
            if p.variant.is_some() && !p.armed {
                if let Some(pane) = s.cleanup.pane.as_mut() {
                    pane.armed = true;
                }
                s.status = Some("press y again to run it".into());
                return Vec::new();
            }
            return start(s, &it, p.variant, false);
        }
        Intent::Close | Intent::OtherForm | Intent::Run => {}
    }
    Vec::new()
}

#[cfg(test)]
mod tests;

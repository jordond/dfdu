//! The marks pane's state: whether it is open, its cursor over the marks, and the estimate of
//! what deleting them would free ([`Command::Estimate`]), asked for when the pane opens and
//! again after each change of the marks ([`ask`]). It opens in the place of the details pane.

use dfdu_engine::{Command, Estimate};
use dfdu_tree::NodeRef;

use crate::effect::Effect;
use crate::state::SessionState;

#[derive(Clone, Debug, Default)]
pub struct MarksPane {
    /// The pane is open, in the place of the details pane, with the focus.
    pub open: bool,
    /// The selected item, an index into the marks.
    pub cursor: usize,
    /// The id of the last estimate asked for, and the outermost marks it was asked for.
    estimate: u64,
    asked: Vec<NodeRef>,
}

/// The pane opens on the first item and asks what the marks would free.
pub fn open(s: &mut SessionState) -> Vec<Effect> {
    s.marks_pane = MarksPane {
        open: true,
        ..MarksPane::default()
    };
    ask(s)
}

/// Closes the pane; false when it was not open.
pub fn close(s: &mut SessionState) -> bool {
    std::mem::replace(&mut s.marks_pane.open, false)
}

/// The marked nodes a delete would send: the outermost marks.
fn outermost(s: &SessionState) -> Vec<NodeRef> {
    (s.marks.items().iter())
        .filter(|it| !it.inside)
        .filter_map(|it| it.target.node)
        .collect()
}

/// Asks the engine what deleting the outermost marks would free, when they changed since the
/// last ask.
pub fn ask(s: &mut SessionState) -> Vec<Effect> {
    let items = outermost(s);
    if items.is_empty() || items == s.marks_pane.asked {
        return Vec::new();
    }
    s.estimates += 1;
    s.marks_pane.estimate = s.estimates;
    s.marks_pane.asked.clone_from(&items);
    vec![Effect::Send(Command::Estimate {
        id: s.estimates,
        items,
    })]
}

/// The estimate for the marks as they are now: the one the pane asked for, or a later one (the
/// delete dialog asks for the same marks), while the marks are those asked for.
pub fn estimate(s: &SessionState) -> Option<&Estimate> {
    let p = &s.marks_pane;
    (s.snap.estimate.as_ref())
        .filter(|e| p.estimate > 0 && e.id >= p.estimate && outermost(s) == p.asked)
}

/// Moves the cursor by `delta` items, kept on one of them.
pub fn move_by(s: &mut SessionState, delta: isize) {
    let last = s.marks.items().len().saturating_sub(1);
    let p = &mut s.marks_pane;
    p.cursor = p.cursor.saturating_add_signed(delta).min(last);
}

/// Unmarks the selected item; the cursor stays on the row, now the next item.
pub fn unmark(s: &mut SessionState) {
    if let Some(it) = s.marks.items().get(s.marks_pane.cursor).cloned() {
        super::set(s, it, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::test_entry;
    use crate::state::tests::browser;

    /// Opening asks for an estimate of the marks, once per change of them; the cursor stays on
    /// one of them as they are unmarked.
    #[test]
    fn the_pane_asks_once_per_change_and_keeps_its_cursor() {
        let mut s = browser(
            (0..3)
                .map(|i| test_entry(i, &format!("f{i}"), 10))
                .collect(),
        );
        assert!(open(&mut s).is_empty(), "nothing marked, nothing to ask");
        assert!(s.marks_pane.open);
        for _ in 0..3 {
            crate::marks::toggle(&mut s);
        }
        let asked = open(&mut s);
        assert!(
            matches!(&asked[..], [Effect::Send(Command::Estimate { id: 1, items })] if items.len() == 3),
            "{asked:?}"
        );
        assert!(ask(&mut s).is_empty(), "the same marks");
        assert!(estimate(&s).is_none(), "none came in yet");
        move_by(&mut s, 5);
        assert_eq!(s.marks_pane.cursor, 2);
        unmark(&mut s);
        move_by(&mut s, 0);
        assert_eq!((s.marks.items().len(), s.marks_pane.cursor), (2, 1));
        assert!(matches!(
            &ask(&mut s)[..],
            [Effect::Send(Command::Estimate { id: 2, .. })]
        ));
        assert!(close(&mut s) && !s.marks_pane.open);
        assert!(!close(&mut s), "closed already");
    }
}

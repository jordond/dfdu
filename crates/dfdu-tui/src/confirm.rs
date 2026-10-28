//! The delete confirmation, for the marked entries or else the selected one. It lists the
//! targets, their total and the method: `y` moves them to the Trash, `p` switches between the
//! Trash and a permanent delete, `s` between the Trash and staging (renamed aside at once, put
//! back with `U` for a minute, then deleted). A permanent delete, and a move to the Trash of 50 GiB or more
//! or of a directory still being scanned, needs the word "delete" typed and `Enter`.

use std::cmp::Reverse;

use dfdu_engine::{Command, Mode};
use dfdu_tree::NodeRef;
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};

use crate::app::{App, Effect, Overlay};
use crate::input::{Edit, LineInput};
use crate::snapshot::{EntryData, Mark};
use crate::theme::{count, human};

/// The word a typed confirmation asks for.
const WORD: &str = "delete";
/// Longest text the confirmation input takes.
const TYPED_MAX: usize = 16;
/// A move to the Trash of this much or more also needs the word typed.
const LARGE: u64 = 50 << 30;
/// Targets listed before "and N more".
const SHOWN: usize = 8;

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

    fn label(&self) -> String {
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

impl Confirm {
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

    /// A target sent is still being scanned: `total` is a lower bound.
    fn scanning(&self) -> bool {
        self.sent().any(|t| t.scanning)
    }

    fn sent(&self) -> impl Iterator<Item = &Item> {
        self.targets.iter().filter(|t| !t.inside)
    }
}

/// Asks to delete the marked entries, the outermost of them, or without marks the selected
/// entry, and asks the engine what that would free. The "shared between entries" row and
/// deleted rows cannot be deleted.
pub fn open(app: &mut App, mode: Mode) -> Vec<Effect> {
    let targets: Vec<Item> = if app.marks.is_empty() {
        let e = app.selected_entry().filter(|e| !e.pseudo);
        e.map(Item::of).into_iter().collect()
    } else {
        let dir = &app.snap.dir_path;
        app.marks
            .items()
            .iter()
            .filter_map(|m| {
                let path = &m.target.path;
                let shown = match path.strip_prefix(dir) {
                    Ok(rel) if !rel.as_os_str().is_empty() => rel,
                    _ => path,
                };
                Some(Item {
                    node: m.target.node?,
                    name: shown.display().to_string(),
                    size: m.size,
                    items: m.items,
                    shared_out: m.shared_out,
                    is_dir: m.is_dir,
                    scanning: m.scanning,
                    inside: m.inside,
                })
            })
            .collect()
    };
    if targets.is_empty() {
        return Vec::new();
    }
    let mut c = Confirm::new(targets, mode);
    app.estimates += 1;
    c.estimate = app.estimates;
    let items = c.sent().map(|t| t.node).collect();
    app.overlay = Overlay::ConfirmDelete(c);
    vec![Effect::Send(Command::Estimate {
        id: app.estimates,
        items,
    })]
}

/// Handles a key while the dialog is open: confirms, switches the method, cancels or edits the
/// typed word. Confirming sends one delete of every target that is not inside another.
pub fn key(app: &mut App, k: KeyEvent) -> Vec<Effect> {
    let Overlay::ConfirmDelete(c) = &mut app.overlay else {
        return Vec::new();
    };
    let confirmed = match k.code {
        KeyCode::Esc | KeyCode::Char('n') => false,
        KeyCode::Char('p' | 'P') => {
            let other = match c.mode {
                Mode::Trash | Mode::Stage => Mode::Permanent,
                Mode::Permanent => Mode::Trash,
            };
            c.set_mode(other);
            return Vec::new();
        }
        KeyCode::Char('s' | 'S') if !c.typing || c.typed.text().is_empty() => {
            let other = match c.mode {
                Mode::Stage => Mode::Trash,
                _ => Mode::Stage,
            };
            c.set_mode(other);
            return Vec::new();
        }
        _ if !c.typing => matches!(k.code, KeyCode::Char('y' | 'Y')),
        KeyCode::Char(_) if c.typed.text().chars().count() >= TYPED_MAX => return Vec::new(),
        _ => match c.typed.key(&k) {
            Edit::Submit => {
                if c.typed.text() != WORD {
                    app.status = Some("not deleted: type delete to confirm".into());
                }
                c.typed.text() == WORD
            }
            Edit::Cancel => false,
            Edit::Changed | Edit::Moved | Edit::Ignored => return Vec::new(),
        },
    };
    let mut fx = Vec::new();
    if confirmed {
        let mode = c.mode;
        fx.push(Effect::Send(Command::Delete {
            items: c.sent().map(|t| t.node).collect(),
            mode,
        }));
        app.status = Some(match mode {
            Mode::Trash => "moving to the Trash…".into(),
            Mode::Stage => "staging…".into(),
            Mode::Permanent => "deleting…".into(),
        });
    }
    app.overlay = Overlay::None;
    fx
}

/// Draws the dialog centred in `area` and returns the rectangle it covers.
pub fn draw(f: &mut Frame, app: &App, area: Rect) -> Rect {
    let Overlay::ConfirmDelete(c) = &app.overlay else {
        return Rect::default();
    };
    let t = &app.theme;
    let sent: Vec<&Item> = c.sent().collect();
    let what = match sent.as_slice() {
        [one] => one.label(),
        _ => format!("{} entries", sent.len()),
    };
    let mut lines = match c.mode {
        Mode::Trash => vec![Line::raw(format!(" Move {what} to the Trash?"))],
        Mode::Stage => vec![
            Line::raw(format!(" Stage {what} for deletion?")),
            Line::raw(format!(
                " Deleted after {} s; U puts it back until then.",
                dfdu_engine::STAGE_UNDO_WINDOW.as_secs()
            )),
        ],
        Mode::Permanent => vec![
            Line::styled(format!(" Permanently delete {what}?"), t.error),
            Line::raw(" This cannot be undone."),
        ],
    };
    if let [one] = c.targets.as_slice() {
        lines.push(Line::styled(
            if one.scanning {
                format!(" at least {} (still scanning)", human(one.size))
            } else {
                format!(" {}", human(one.size))
            },
            t.dim,
        ));
    } else {
        for it in c.targets.iter().take(SHOWN) {
            let mut spans = vec![Span::raw(format!(
                " {:>10}  {}",
                human(it.size),
                it.label()
            ))];
            if it.inside {
                spans.push(Span::styled("  inside another marked item", t.dim));
            } else if it.scanning {
                spans.push(Span::styled("  still scanning: at least this", t.dim));
            }
            lines.push(Line::from(spans));
        }
        if c.targets.len() > SHOWN {
            lines.push(Line::styled(
                format!(" {:>10}  and {} more", "", c.targets.len() - SHOWN),
                t.dim,
            ));
        }
        let mut total = vec![Span::raw(format!(
            " {:>10}  total · {} items",
            human(c.total),
            count(c.items)
        ))];
        if c.scanning() {
            total.push(Span::styled("  still scanning: at least this", t.dim));
        }
        lines.push(Line::from(total));
    }
    match app.snap.estimate.as_ref().filter(|e| e.id == c.estimate) {
        Some(e) => {
            let mut s = format!(
                " Frees {}{}",
                if e.scanning { "at least " } else { "about " },
                human(e.bytes)
            );
            let stays: Vec<String> = [
                (e.linked_out, "hard-linked from outside"),
                (e.shared_out, "shared with data outside the scan"),
            ]
            .iter()
            .filter(|(b, _)| *b > 0)
            .map(|(b, why)| format!("{} {why}", human(*b)))
            .collect();
            if !stays.is_empty() {
                s.push_str(&format!("; stays: {}", stays.join(", ")));
            }
            if e.may_free_less {
                s.push_str("; may free less (clones)");
            }
            lines.push(Line::styled(
                s,
                if e.linked_out + e.shared_out > 0 {
                    t.warn
                } else {
                    t.dim
                },
            ));
        }
        None if c.shared_out > 0 => lines.push(Line::styled(
            format!(
                " May free less: {} of it is also used outside the scan.",
                human(c.shared_out)
            ),
            t.warn,
        )),
        None => {}
    }
    lines.push(Line::raw(""));
    let (method, other) = match c.mode {
        Mode::Trash => ("Trash, recoverable", "delete permanently"),
        Mode::Stage => ("stage, then delete", "delete permanently"),
        Mode::Permanent => ("delete permanently", "move to the Trash"),
    };
    let staging = if c.mode == Mode::Stage {
        "the Trash"
    } else {
        "stage"
    };
    lines.push(Line::from(vec![
        Span::raw(format!(" Method: {method}   ")),
        Span::styled("p", t.key),
        Span::raw(format!(" {other}  ")),
        Span::styled("s", t.key),
        Span::raw(format!(" {staging}")),
    ]));
    if c.mode != Mode::Permanent && c.typing {
        let why = if c.total >= LARGE {
            format!(" {} or more: confirm by typing the word.", human(LARGE))
        } else {
            format!(
                " Still scanning, may be {} or more: confirm by typing the word.",
                human(LARGE)
            )
        };
        lines.push(Line::styled(why, t.dim));
    }
    lines.push(if c.typing {
        let mut spans = vec![
            Span::raw(" type "),
            Span::styled(WORD, t.key),
            Span::raw(" and Enter: "),
        ];
        spans.extend(c.typed.spans(Style::new(), t.key));
        spans.extend([
            Span::raw("   "),
            Span::styled("Esc", t.key),
            Span::raw(" cancel"),
        ]);
        Line::from(spans)
    } else {
        Line::from(vec![
            Span::styled(" y", t.key),
            Span::raw(" confirm   "),
            Span::styled("n Esc", t.key),
            Span::raw(" cancel"),
        ])
    });
    // Wide enough for the longest line (a long name, the "may free less" note), at least 60.
    let width = lines
        .iter()
        .map(|l| l.width() + 3)
        .max()
        .unwrap_or(0)
        .max(60);
    let r = area.centered(
        Constraint::Length(width.min(u16::MAX as usize) as u16),
        Constraint::Length(lines.len() as u16 + 2),
    );
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" Delete ")),
        r,
    );
    r
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Instant;

    use super::*;
    use crate::app::tests::snap;
    use crate::snapshot::{Snapshot, test_entry};
    use crate::tests::{key, render};
    use crate::theme::Theme;

    fn dialog(app: &App) -> &Confirm {
        match &app.overlay {
            Overlay::ConfirmDelete(c) => c,
            other => panic!("no dialog: {other:?}"),
        }
    }

    fn type_word(app: &mut App, t: Instant) {
        for c in WORD.chars() {
            assert!(app.key(key(KeyCode::Char(c)), t).is_empty());
        }
    }

    /// Twelve entries of 1 to 12 MiB, all marked.
    fn twelve_marked(t: Instant) -> App {
        let mut app = App::new(Theme::no_color(), t);
        let es = (0..12)
            .map(|i| test_entry(i, &format!("e{i:02}"), (i as u64 + 1) << 20))
            .collect();
        app.refresh(snap(es, 1), t);
        for _ in 0..12 {
            app.key(key(KeyCode::Char(' ')), t);
        }
        assert_eq!(app.marks.items().len(), 12);
        app
    }

    #[test]
    fn trash_delete_sends_a_plan_after_y() {
        let t0 = Instant::now();
        let mut app = App::new(Theme::no_color(), t0);
        app.refresh(snap(vec![test_entry(0, "a", 10)], 1), t0);
        let fx = app.key(key(KeyCode::Char('d')), t0);
        assert!(
            matches!(
                fx.as_slice(),
                [Effect::Send(Command::Estimate { id: 1, items })] if items.len() == 1
            ),
            "the dialog asks what the delete frees: {fx:?}"
        );
        assert!(matches!(
            app.overlay,
            Overlay::ConfirmDelete(Confirm {
                mode: Mode::Trash,
                typing: false,
                ..
            })
        ));
        let fx = app.key(key(KeyCode::Char('y')), t0);
        assert!(matches!(
            fx.as_slice(),
            [Effect::Send(Command::Delete {
                mode: Mode::Trash,
                ..
            })]
        ));
        assert!(matches!(app.overlay, Overlay::None));
    }

    #[test]
    fn permanent_delete_needs_the_word_delete() {
        let t0 = Instant::now();
        let mut app = App::new(Theme::no_color(), t0);
        app.refresh(snap(vec![test_entry(0, "a", 10)], 1), t0);
        app.key(key(KeyCode::Char('D')), t0);
        // 'y' alone does nothing for a permanent delete.
        assert!(app.key(key(KeyCode::Char('y')), t0).is_empty());
        app.key(key(KeyCode::Backspace), t0);
        for c in "delet".chars() {
            assert!(app.key(key(KeyCode::Char(c)), t0).is_empty());
        }
        assert!(
            app.key(key(KeyCode::Enter), t0).is_empty(),
            "incomplete word must not delete"
        );
        assert!(matches!(app.overlay, Overlay::None));
        assert_eq!(
            app.status.as_deref(),
            Some("not deleted: type delete to confirm")
        );
        app.key(key(KeyCode::Char('D')), t0);
        type_word(&mut app, t0);
        let fx = app.key(key(KeyCode::Enter), t0);
        assert!(matches!(
            fx.as_slice(),
            [Effect::Send(Command::Delete {
                mode: Mode::Permanent,
                ..
            })]
        ));
    }

    #[test]
    fn dialog_lists_targets_and_more() {
        let t = Instant::now();
        let mut app = twelve_marked(t);
        app.key(key(KeyCode::Char('d')), t);
        let out = render(&mut app, t, 100, 30);
        assert!(out.contains("Move 12 entries to the Trash?"), "{out}");
        // The eight largest are listed, largest first.
        let listed: Vec<usize> = (0..12)
            .filter_map(|i| out.find(&format!("e{i:02}")))
            .collect();
        assert_eq!(listed.len(), 8, "{out}");
        for i in 4..12 {
            assert!(
                out.contains(&format!("{}  e{i:02}", human((i + 1) << 20))),
                "{out}"
            );
        }
        assert!(out.find("e11").unwrap() < out.find("e04").unwrap(), "{out}");
        assert!(out.contains("and 4 more"), "{out}");
        assert!(
            out.contains(&format!("{}  total · 12 items", human(78 << 20))),
            "{out}"
        );
        assert!(out.contains("Method: Trash, recoverable"), "{out}");
    }

    #[test]
    fn shows_the_engine_estimate() {
        let t = Instant::now();
        let mut app = App::new(Theme::no_color(), t);
        app.refresh(snap(vec![test_entry(0, "a", 10)], 1), t);
        app.key(key(KeyCode::Char('d')), t);
        let mut s = snap(vec![test_entry(0, "a", 10)], 2);
        s.estimate = Some(dfdu_engine::Estimate {
            id: 1,
            bytes: 3 << 20,
            linked_out: 1 << 20,
            ..Default::default()
        });
        app.refresh(s, t);
        let out = render(&mut app, t, 100, 30);
        assert!(
            out.contains("Frees about 3.0 MiB; stays: 1.0 MiB hard-linked from outside"),
            "{out}"
        );
    }

    #[test]
    fn s_switches_to_staging() {
        let t = Instant::now();
        let mut app = App::new(Theme::no_color(), t);
        app.refresh(snap(vec![test_entry(0, "a", 10)], 1), t);
        app.key(key(KeyCode::Char('d')), t);
        assert!(app.key(key(KeyCode::Char('s')), t).is_empty());
        assert_eq!(
            (dialog(&app).mode, dialog(&app).typing),
            (Mode::Stage, false)
        );
        let out = render(&mut app, t, 100, 30);
        assert!(out.contains("Stage a for deletion?"), "{out}");
        assert!(out.contains("U puts it back"), "{out}");
        assert!(out.contains("s the Trash"), "{out}");
        let fx = app.key(key(KeyCode::Char('y')), t);
        assert!(
            matches!(
                fx.as_slice(),
                [Effect::Send(Command::Delete {
                    mode: Mode::Stage,
                    ..
                })]
            ),
            "{fx:?}"
        );
    }

    #[test]
    fn p_switches_method() {
        let t = Instant::now();
        let mut app = App::new(Theme::no_color(), t);
        app.refresh(snap(vec![test_entry(0, "a", 10)], 1), t);
        app.key(key(KeyCode::Char('d')), t);
        assert!(app.key(key(KeyCode::Char('p')), t).is_empty());
        assert_eq!(
            (dialog(&app).mode, dialog(&app).typing),
            (Mode::Permanent, true)
        );
        let out = render(&mut app, t, 100, 30);
        assert!(out.contains("Permanently delete a?"), "{out}");
        assert!(out.contains("p move to the Trash  s stage"), "{out}");
        // Permanent now: 'y' is only typed.
        assert!(app.key(key(KeyCode::Char('y')), t).is_empty());
        assert!(matches!(app.overlay, Overlay::ConfirmDelete(_)));
        // Back to the Trash: the typed text is dropped and 'y' confirms again.
        app.key(key(KeyCode::Char('p')), t);
        assert_eq!(
            (dialog(&app).mode, dialog(&app).typing),
            (Mode::Trash, false)
        );
        assert!(dialog(&app).typed.is_empty());
        let fx = app.key(key(KeyCode::Char('y')), t);
        assert!(matches!(
            fx.as_slice(),
            [Effect::Send(Command::Delete {
                mode: Mode::Trash,
                ..
            })]
        ));
        // From D, p goes to the Trash.
        app.key(key(KeyCode::Char('D')), t);
        app.key(key(KeyCode::Char('p')), t);
        assert_eq!(dialog(&app).mode, Mode::Trash);
        assert!(app.key(key(KeyCode::Char('n')), t).is_empty());
        assert!(matches!(app.overlay, Overlay::None));
    }

    #[test]
    fn large_trash_delete_needs_typing() {
        let t = Instant::now();
        let gib = 1 << 30;
        let mut app = App::new(Theme::no_color(), t);
        app.refresh(
            snap(
                vec![
                    test_entry(0, "big", 50 * gib),
                    test_entry(1, "less", 49 * gib),
                ],
                1,
            ),
            t,
        );
        app.key(key(KeyCode::Char('d')), t);
        assert!(dialog(&app).typing);
        let out = render(&mut app, t, 100, 30);
        assert!(out.contains("Move big to the Trash?"), "{out}");
        assert!(
            out.contains("50.0 GiB or more: confirm by typing the word."),
            "{out}"
        );
        assert!(out.contains("type delete and Enter"), "{out}");
        assert!(app.key(key(KeyCode::Char('y')), t).is_empty());
        assert!(matches!(app.overlay, Overlay::ConfirmDelete(_)));
        app.key(key(KeyCode::Backspace), t);
        type_word(&mut app, t);
        let fx = app.key(key(KeyCode::Enter), t);
        assert!(matches!(
            fx.as_slice(),
            [Effect::Send(Command::Delete {
                mode: Mode::Trash,
                ..
            })]
        ));
        // Just under the threshold, 'y' is enough.
        app.key(key(KeyCode::Down), t);
        app.key(key(KeyCode::Char('d')), t);
        assert!(!dialog(&app).typing);
        assert_eq!(app.key(key(KeyCode::Char('y')), t).len(), 1);
        // Marked entries count together.
        app.key(key(KeyCode::Char(' ')), t);
        app.key(key(KeyCode::Up), t);
        app.key(key(KeyCode::Char(' ')), t);
        app.key(key(KeyCode::Char('d')), t);
        assert!(dialog(&app).typing);
    }

    #[test]
    fn trash_delete_while_scanning_needs_typing() {
        let t = Instant::now();
        let mut app = App::new(Theme::no_color(), t);
        let scanning = EntryData {
            is_dir: true,
            mark: Mark::Scanning,
            ..test_entry(0, "growing", 40 << 30)
        };
        app.refresh(snap(vec![scanning, test_entry(1, "done", 10)], 1), t);
        app.key(key(KeyCode::Char('d')), t);
        assert!(dialog(&app).typing);
        let out = render(&mut app, t, 100, 30);
        assert!(out.contains("at least 40.0 GiB (still scanning)"), "{out}");
        assert!(
            out.contains("Still scanning, may be 50.0 GiB or more: confirm by typing the word."),
            "{out}"
        );
        assert!(app.key(key(KeyCode::Char('y')), t).is_empty());
        app.key(key(KeyCode::Backspace), t);
        type_word(&mut app, t);
        let fx = app.key(key(KeyCode::Enter), t);
        assert!(matches!(
            fx.as_slice(),
            [Effect::Send(Command::Delete {
                mode: Mode::Trash,
                ..
            })]
        ));
        // Marked together with a finished entry, it still needs the word; the finished one
        // alone does not.
        app.key(key(KeyCode::Char(' ')), t);
        app.key(key(KeyCode::Char(' ')), t);
        app.key(key(KeyCode::Char('d')), t);
        assert!(dialog(&app).typing);
        let out = render(&mut app, t, 100, 30);
        assert!(out.contains("Move 2 entries to the Trash?"), "{out}");
        assert!(out.contains("Still scanning, may be"), "{out}");
        app.key(key(KeyCode::Esc), t);
        crate::marks::clear(&mut app);
        app.key(key(KeyCode::Char('d')), t);
        assert!(!dialog(&app).typing);
    }

    #[test]
    fn one_command_with_all_outermost_targets() {
        let t = Instant::now();
        let mut app = App::new(Theme::no_color(), t);
        let a = EntryData {
            is_dir: true,
            ..test_entry(0, "a", 5000)
        };
        let (b, c) = (test_entry(1, "b", 300), test_entry(2, "c", 200));
        app.refresh(snap(vec![a.clone(), b.clone(), c.clone()], 1), t);
        app.key(key(KeyCode::Char(' ')), t);
        app.key(key(KeyCode::Char(' ')), t);
        // Into a/, and mark x there.
        app.key(key(KeyCode::Up), t);
        app.key(key(KeyCode::Up), t);
        app.key(key(KeyCode::Enter), t);
        let x = test_entry(3, "x", 4000);
        app.refresh(
            Snapshot {
                dir_path: PathBuf::from("/r/a"),
                dir_node: Some(a.node),
                entries: vec![x.clone()],
                version: 2,
                ..Default::default()
            },
            t,
        );
        app.key(key(KeyCode::Char(' ')), t);
        app.key(key(KeyCode::Char('d')), t);
        let out = render(&mut app, t, 100, 30);
        assert!(out.contains("Move 2 entries to the Trash?"), "{out}");
        // Seen from a/, the others are shown by their full path.
        assert!(out.contains("/r/a/"), "{out}");
        assert!(out.contains("/r/b"), "{out}");
        assert!(out.contains("x  inside another marked item"), "{out}");
        assert!(
            out.contains(&format!("{}  total · 2 items", human(5300))),
            "{out}"
        );
        let fx = app.key(key(KeyCode::Char('y')), t);
        let [Effect::Send(Command::Delete { items, mode })] = fx.as_slice() else {
            panic!("expected one delete: {fx:?}");
        };
        assert_eq!(
            (items.as_slice(), *mode),
            ([a.node, b.node].as_slice(), Mode::Trash)
        );
    }

    #[test]
    fn dialog_fits_a_small_terminal() {
        let t = Instant::now();
        let mut app = twelve_marked(t);
        for k in ['d', 'D'] {
            app.key(key(KeyCode::Char(k)), t);
            // Smaller than the dialog: the dialog is clamped to the screen.
            let out = render(&mut app, t, 40, 8);
            assert_eq!(out.lines().count(), 8, "{out}");
            assert!(out.contains('┌'), "{out}");
            let out = render(&mut app, t, 100, 30);
            assert!(out.contains('┌') && out.contains('┘'), "{out}");
            assert!(out.contains("and 4 more"), "{out}");
            app.key(key(KeyCode::Esc), t);
            assert!(matches!(app.overlay, Overlay::None));
        }
    }
}

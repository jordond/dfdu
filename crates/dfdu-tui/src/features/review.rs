//! What the review views share: Largest, Duplicates and Cleanup all list candidates, let the
//! user tick some and delete what is ticked, so they look and behave alike.
//!
//! Ticks are the list's marks: ticking an item marks it, and the marks pane, the footer and the
//! delete dialog see it as any other mark. Each view starts with the summary bar
//! ([`summary`]): what is ticked against what it lists, and the key that deletes it. Rows start
//! with a checkbox ([`Tick`]) that reads without colour, give ages relative to now ([`age`]),
//! and shorten paths in the middle while keeping their last two components, with the part all
//! rows share dimmed ([`path`]). A view with a detail pane puts it on the right of the rows on
//! wide terminals and below them otherwise ([`split`]).
//!
//! The keys are the registry's: `Space` ticks, `A` ticks what the view suggests, `X` unticks
//! everything. `d` and `D` delete what is ticked among what the view lists, the marks made
//! elsewhere left alone, so the delete matches the summary bar; with nothing ticked there they
//! say how to tick instead of deleting the list's selection hidden behind the view
//! ([`delete`], which the registry runs, from a key or from the palette alike).

use dfdu_engine::Mode;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::app::keys::{Action, first_key};
use crate::app::views::{self, View};
use crate::app::{App, Effect};
use crate::features::{cleanup, dupes, largest};
use crate::format::{human, truncate_middle};
use crate::overlay::confirm;
use crate::theme::Theme;

mod detail;
mod text;

pub use detail::{draw_detail, on_right, split};
pub use text::{age, path, shared_prefix};

/// Cells of the checkbox column, its gap included.
pub const CHECK_WIDTH: usize = 4;

/// What the summary bar counts: the size and number of the items ticked, of those listed.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Tally {
    pub ticked_size: u64,
    pub size: u64,
    pub ticked: usize,
    pub items: usize,
}

impl Tally {
    /// Counts one item of `size`, ticked or not.
    pub fn add(&mut self, size: u64, ticked: bool) {
        self.size += size;
        self.items += 1;
        if ticked {
            self.ticked_size += size;
            self.ticked += 1;
        }
    }
}

/// The summary bar's style: the text colour on the subtle shade of an empty bar, bold without
/// colours.
pub fn bar_style(t: &Theme) -> Style {
    let mut s = Style::new().add_modifier(Modifier::BOLD);
    if let Some(fg) = t.fg.fg {
        s = s.fg(fg);
    }
    if let Some(bg) = t.bar_empty.fg {
        s = s.bg(bg);
    }
    s
}

/// The summary bar, `width` cells: `Selected 2.4 GiB of 3.1 GiB · 6 of 14 items` and what the
/// view adds (`extra`) on the left, the key that deletes on the right (left out in a read-only
/// browser). `noun` names what is counted, in the plural.
pub fn summary(app: &App, tally: Tally, noun: &str, extra: &str, width: u16) -> Line<'static> {
    let t = &app.theme;
    let selected = format!(
        " Selected {} of {} · {} of {} {noun}",
        human(tally.ticked_size),
        human(tally.size),
        tally.ticked,
        tally.items
    );
    let right = if app.read_only {
        String::new()
    } else {
        format!("{} delete selected ", first_key(Action::Trash))
    };
    let rw = Span::raw(&right).width();
    let room = usize::from(width).saturating_sub(rw + 1);
    // What the view adds gives way first.
    let left = format!("{selected}{extra}");
    let left = if Span::raw(&left).width() <= room {
        left
    } else {
        truncate_middle(&selected, room)
    };
    let pad = usize::from(width).saturating_sub(Span::raw(&left).width() + rw);
    Line::from(vec![
        Span::raw(left),
        Span::raw(" ".repeat(pad)),
        Span::styled(right, Style::new().add_modifier(Modifier::BOLD)),
    ])
    .style(bar_style(t))
}

/// The state of a row's checkbox.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Tick {
    On,
    Off,
    /// A section with some of its items ticked.
    Partial,
    /// The copy of a group of duplicates that is kept.
    Keep,
    /// Cannot be ticked (protected).
    Locked,
}

/// The checkbox, [`CHECK_WIDTH`] cells: `[x]`, `[ ]`, `[-]`, `KEEP` or `[⊘]`.
pub fn checkbox(t: &Theme, tick: Tick) -> Span<'static> {
    match tick {
        Tick::On => Span::styled("[x] ", t.mark),
        Tick::Off => Span::styled("[ ] ", t.fg_dim),
        Tick::Partial => Span::styled("[-] ", t.mark),
        Tick::Keep => Span::styled("KEEP", t.status_ok.add_modifier(Modifier::BOLD)),
        Tick::Locked => Span::styled("[⊘] ", t.fg_dim),
    }
}

/// The tick of a section whose items have `ticked` of `of` ticked.
pub fn tick_of(ticked: usize, of: usize) -> Tick {
    match ticked {
        0 => Tick::Off,
        n if n >= of => Tick::On,
        _ => Tick::Partial,
    }
}

/// `d` and `D` (in `mode`): in a review view, the delete dialog for what is ticked among what
/// the view lists, and only that; with nothing ticked there, or in Duplicates with every copy
/// of a group ticked, the status line says why instead. In the other views, the list's delete.
pub fn delete(app: &mut App, mode: Mode) -> Vec<Effect> {
    let listed = match views::current(app) {
        View::Files | View::Map => return confirm::open(app, mode),
        View::Largest => largest::listed(app),
        View::Cleanup => cleanup::listed(app),
        View::Duplicates => dupes::listed(app),
    };
    let ticked = (app.marks.items().iter())
        .filter_map(|m| m.target.node)
        .any(|n| listed.contains(&n));
    if !ticked {
        app.status = Some(format!(
            "tick items with {} first: {} and {} delete the ticked ones",
            first_key(Action::ToggleMark),
            first_key(Action::Trash),
            first_key(Action::DeletePermanent)
        ));
        return Vec::new();
    }
    if views::current(app) == View::Duplicates
        && let Some(name) = dupes::whole_group_ticked(app)
    {
        app.status = Some(format!(
            "every copy of {name} is ticked: untick one to keep it ({} unticks)",
            first_key(Action::ToggleMark)
        ));
        return Vec::new();
    }
    confirm::open_only(app, mode, &listed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections_tick_partly() {
        assert_eq!(tick_of(0, 3), Tick::Off);
        assert_eq!(tick_of(2, 3), Tick::Partial);
        assert_eq!(tick_of(3, 3), Tick::On);
        let t = Theme::no_color();
        for tick in [Tick::On, Tick::Off, Tick::Partial, Tick::Keep, Tick::Locked] {
            assert_eq!(checkbox(&t, tick).width(), CHECK_WIDTH, "{tick:?}");
        }
    }

    #[test]
    fn the_summary_bar_counts_the_ticked_of_the_listed() {
        let mut tally = Tally::default();
        tally.add(1 << 20, true);
        tally.add(1 << 20, false);
        assert_eq!(
            tally,
            Tally {
                ticked_size: 1 << 20,
                size: 2 << 20,
                ticked: 1,
                items: 2
            }
        );
        let mut app = App::new(Theme::no_color(), std::time::Instant::now());
        let text = |app: &App, width| {
            let line = summary(app, tally, "files", " · 3 groups", width);
            assert_eq!(line.width(), usize::from(width));
            line.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        };
        let wide = text(&app, 80);
        assert!(
            wide.starts_with(" Selected 1.0 MiB of 2.0 MiB · 1 of 2 files · 3 groups"),
            "{wide}"
        );
        assert!(wide.ends_with("d delete selected "), "{wide}");
        // What the view adds gives way first.
        let narrow = text(&app, 60);
        assert!(!narrow.contains("groups"), "{narrow}");
        assert!(narrow.contains("1 of 2 files"), "{narrow}");
        app.read_only = true;
        assert!(!text(&app, 80).contains("delete"));
    }
}

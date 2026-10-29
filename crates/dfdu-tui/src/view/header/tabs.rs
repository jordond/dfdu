//! The tab bar on the header's first line: the five views by key and name, the one shown
//! highlighted, in the forms the header picks from by width ([`tabs`]).

use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use crate::app::App;
use crate::app::keys::first_key;
use crate::app::views::{self, View};
use crate::theme::{Bars, Theme};

/// The tab bar names every view from this many columns; below, only the one shown.
pub const TABS_NAMED_FROM: u16 = 80;

/// Between two tabs, and between the title and the tabs; `|` with [`Bars::Ascii`].
fn tab_separator(bars: Bars) -> &'static str {
    match bars {
        Bars::Unicode => "│",
        Bars::Ascii => "|",
    }
}

/// One form of the tab bar: its spans after the title, and the column offset and width of each
/// tab in them.
pub struct Tabs {
    pub spans: Vec<Span<'static>>,
    pub hits: Vec<(usize, usize, View)>,
}

/// The tab of the view shown: the focus colour as its background, bold. Reverse video carries
/// it without colour too.
fn active_tab(t: &Theme) -> Style {
    t.border_focus
        .add_modifier(Modifier::REVERSED | Modifier::BOLD)
}

/// The tab bar after the title. `named`: every view by key and name, separated by `│`, with
/// what the view holds after the name when `badges`. Otherwise the keys alone and the view
/// shown named in brackets: `1 2 [3 Cleanup] 4 5`.
pub fn tabs(app: &App, named: bool, badges: bool) -> Tabs {
    let t = &app.theme;
    let shown = views::current(app);
    let (mut spans, mut hits, mut x) = (Vec::new(), Vec::new(), 0);
    for (i, view) in View::ALL.into_iter().enumerate() {
        let key = first_key(view.action());
        let active = view == shown;
        let text = if named {
            let badge = (badges.then(|| views::badge(app, view)).flatten())
                .map(|b| format!(" {b}"))
                .unwrap_or_default();
            format!(" {key} {}{badge} ", view.name())
        } else if active {
            format!("[{key} {}]", view.name())
        } else {
            key.to_owned()
        };
        let sep = match (named, i) {
            (true, _) => tab_separator(t.bars).to_owned(),
            (false, 0) => format!("{} ", tab_separator(t.bars)),
            (false, _) => " ".to_owned(),
        };
        x += Span::raw(&sep).width();
        spans.push(Span::styled(sep, t.fg_dim));
        let w = Span::raw(&text).width();
        hits.push((x, w, view));
        spans.push(Span::styled(
            text,
            if active { active_tab(t) } else { t.fg_dim },
        ));
        x += w;
    }
    Tabs { spans, hits }
}

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

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::tests::library_snap;

    fn text(spans: &[Span]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn app(theme: Theme) -> App {
        let t0 = Instant::now();
        let mut app = App::new(theme, t0);
        app.refresh(library_snap(), t0);
        app
    }

    #[test]
    fn named_tabs_list_every_view_and_short_ones_name_the_one_shown() {
        let app = app(Theme::no_color());
        let named = tabs(&app, true, false);
        assert_eq!(
            text(&named.spans),
            "│ 1 Files │ 2 Map │ 3 Cleanup │ 4 Duplicates │ 5 Largest "
        );
        let short = tabs(&app, false, false);
        assert_eq!(text(&short.spans), "│ [1 Files] 2 3 4 5");
        let views: Vec<View> = short.hits.iter().map(|h| h.2).collect();
        assert_eq!(views, View::ALL);
    }

    #[test]
    fn each_hit_is_where_its_tab_is_drawn() {
        let app = app(Theme::no_color());
        for named in [true, false] {
            let bar = tabs(&app, named, false);
            let line = text(&bar.spans);
            for &(x, w, view) in &bar.hits {
                let at: String = line.chars().skip(x).take(w).collect();
                assert!(at.contains(first_key(view.action())), "{at:?} for {view:?}");
                assert_eq!(Span::raw(&at).width(), w);
            }
        }
    }

    #[test]
    fn the_tab_shown_stands_out_and_ascii_has_its_own_separator() {
        let mut theme = Theme::no_color();
        theme.bars = Bars::Ascii;
        let app = app(theme);
        let bar = tabs(&app, true, false);
        assert!(text(&bar.spans).starts_with("| 1 Files |"));
        let files = bar
            .spans
            .iter()
            .find(|s| s.content == " 1 Files ")
            .expect("Files");
        assert_eq!(files.style, active_tab(&app.theme));
        let map = bar
            .spans
            .iter()
            .find(|s| s.content == " 2 Map ")
            .expect("Map");
        assert_eq!(map.style, app.theme.fg_dim);
    }
}

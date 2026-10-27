//! The debug panel (`F12`): pipeline counters, the byte budget, lock times and frame costs.
//!
//! The counters come with every read of the model. `Timing` is not part of the model's
//! fingerprint, so the panel adds no frames of its own: it shows what the last frame read.

use std::path::PathBuf;
use std::time::Duration;

use dfdu_engine::{Engine, Model, Stats, Timing};
use dfdu_scan::{Backend, Budget};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};

use crate::app::App;
use crate::theme::{Theme, count, human};

/// Stalled directories listed by path.
const STALLED_SHOWN: usize = 3;
const WIDTH: u16 = 48;
/// Below this terminal size the panel is not drawn.
const MIN_AREA: (u16, u16) = (50, 16);

#[derive(Clone, Debug, Default)]
pub struct Debug {
    pub open: bool,
    /// How long the last frame took to draw.
    pub draw: Duration,
    /// How long the last frame's read held the read lock.
    pub read: Duration,
}

/// Counters kept outside the model, read with it.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Counters {
    /// Scanner messages sent to the applier ([`Engine::sent_messages`]).
    pub sent: u64,
    /// Scanner tasks not finished, and how many of them were abandoned.
    pub outstanding: (u64, u64),
    /// Published tasks not yet taken: `(priority, normal)`.
    pub queued: (usize, usize),
}

impl Counters {
    /// Reads the engine's counters; cheap, and done under the read lock so that `sent` is never
    /// below the messages the model has received.
    pub fn of<B: Backend>(engine: &Engine<B>) -> Self {
        Counters {
            sent: engine.sent_messages(),
            outstanding: engine.scanner().outstanding(),
            queued: engine.scanner().queued(),
        }
    }
}

/// What [`read`] copied under the read lock.
#[derive(Clone, Debug, Default)]
pub struct Info {
    pub stats: Stats,
    pub timing: Timing,
    pub counters: Counters,
    /// Bytes in transit, the most at once so far, and the budget's capacity.
    pub budget: (usize, usize, usize),
    pub running: usize,
    pub stalled: usize,
    /// The first [`STALLED_SHOWN`] stalled directories.
    pub stalled_paths: Vec<PathBuf>,
}

/// Opens or closes the panel.
pub fn toggle(app: &mut App) {
    app.debug.open = !app.debug.open;
}

/// Draws the panel in the top-right corner of `body` (the list and the details pane), over the
/// list, if it is open and the terminal is large enough. It takes no keys.
pub fn draw(f: &mut Frame, app: &App, body: Rect) {
    let screen = f.area();
    if !app.debug.open || screen.width < MIN_AREA.0 || screen.height < MIN_AREA.1 {
        return;
    }
    // The list starts where `body` does: this keeps the panel left of a details pane on the
    // right, and lets it reach over a sheet below the list.
    let area = Rect {
        width: app.areas.list.width.min(body.width),
        ..body
    };
    let lines = lines(
        &app.extras.debug,
        &app.theme,
        Some((app.debug.draw, app.debug.read)),
    );
    let width = WIDTH.min(area.width);
    let height = (lines.len() as u16 + 2).min(area.height);
    let r = Rect::new(area.right() - width, area.y, width, height);
    f.render_widget(Clear, r);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(app.theme.border)
        .title(Span::styled(" Debug ", app.theme.title));
    f.render_widget(Paragraph::new(lines).block(block), r);
}

/// Copies the counters; runs under the read lock. Read whether the panel is open or not (a copy
/// of two histograms and at most [`STALLED_SHOWN`] paths), so it has numbers as soon as it
/// opens.
pub fn read(m: &Model, budget: &Budget, counters: Counters) -> Info {
    let s = &m.status;
    Info {
        stats: s.stats.clone(),
        timing: s.timing.clone(),
        counters,
        budget: (budget.used(), budget.peak(), budget.capacity()),
        running: s.running(),
        stalled: s.stalled.len(),
        stalled_paths: s
            .stalled
            .iter()
            .filter_map(|st| m.tree.resolve_visible(st.node))
            .take(STALLED_SHOWN)
            .map(|ix| m.tree.path_buf(ix))
            .collect(),
    }
}

/// Records how long the last frame took to draw and its model read held the lock.
pub fn frame_drawn(app: &mut App, draw: Duration, read: Duration) {
    app.debug.draw = draw;
    app.debug.read = read;
}

/// The panel's numbers as text, for `--debug-stats`: every line but the UI's frame costs.
/// Takes the read lock.
pub fn summary<B: Backend>(engine: &Engine<B>) -> String {
    let info = engine.read(|m| read(m, engine.budget(), Counters::of(engine)));
    text(&info)
}

fn text(info: &Info) -> String {
    let mut out = String::new();
    for line in lines(info, &Theme::no_color(), None) {
        let s: String = line.spans.iter().map(|s| &*s.content).collect();
        out.push_str(s.trim_start());
        out.push('\n');
    }
    out
}

/// A duration in µs below 1 ms, in ms below 10 s, else in seconds.
fn dur(d: Duration) -> String {
    let us = d.as_micros();
    match us {
        0..1_000 => format!("{us}µs"),
        1_000..10_000_000 => format!("{:.1}ms", us as f64 / 1e3),
        _ => format!("{:.1}s", d.as_secs_f64()),
    }
}

/// The panel's lines; the UI's frame costs as the last line when `ui` is given.
fn lines(info: &Info, t: &Theme, ui: Option<(Duration, Duration)>) -> Vec<Line<'static>> {
    let s = &info.stats;
    let tm = &info.timing;
    let c = &info.counters;
    let plain = |text: String| Line::from(format!(" {text}"));
    // An anomaly is marked with `!` as well as the error style, so it shows without colour.
    let anomaly = |name: &str, n: u64| {
        if n > 0 {
            Span::styled(format!("{name} {}!", count(n)), t.error)
        } else {
            Span::raw(format!("{name} 0"))
        }
    };
    let locked = Duration::from_nanos(tm.locked_ns);
    let rate = s.entries as f64 / locked.as_secs_f64().max(1e-9);
    let (used, peak, capacity) = info.budget;
    let mut lines = vec![
        plain(format!(
            "sent {}  received {}  pending {}",
            count(c.sent),
            count(s.messages),
            count(c.sent.saturating_sub(s.messages))
        )),
        plain(format!(
            "chunks {}  closes {}  entries {}",
            count(s.chunks),
            count(s.closes),
            count(s.entries)
        )),
        plain(format!(
            "discarded job {}  cut {}  late {}",
            count(s.discarded_job),
            count(s.discarded_cut),
            count(s.late)
        )),
        Line::from(vec![
            Span::raw(" "),
            anomaly("corrupt", s.corrupt),
            Span::raw("  "),
            anomaly("lost", s.lost),
            Span::raw("  "),
            anomaly("duplicate", s.duplicate),
        ]),
        plain(format!(
            "transit {} / {}  peak {}",
            human(used as u64),
            human(capacity as u64),
            human(peak as u64)
        )),
        plain(format!(
            "tasks {} ({} abandoned)  queued {} + {}",
            count(c.outstanding.0),
            count(c.outstanding.1),
            count(c.queued.0 as u64),
            count(c.queued.1 as u64)
        )),
        plain(format!(
            "lock {}  {} slices  {} entries/s",
            dur(locked),
            count(tm.slices),
            count(rate as u64)
        )),
        plain(format!(
            "hold p50 <{}  p99 <{}  max <{}",
            dur(Timing::percentile(&tm.hold, 0.5)),
            dur(Timing::percentile(&tm.hold, 0.99)),
            dur(Timing::max(&tm.hold))
        )),
        plain(format!(
            "over 2ms {}  wait p99 <{}  max <{}",
            count(Timing::at_least(&tm.hold, Duration::from_millis(2))),
            dur(Timing::percentile(&tm.wait, 0.99)),
            dur(Timing::max(&tm.wait))
        )),
        plain(format!(
            "jobs {} running  stall reports {}",
            info.running,
            count(s.stalled)
        )),
        Line::from(vec![
            Span::raw(" "),
            Span::styled(
                format!("stalled dirs {}", info.stalled),
                if info.stalled > 0 {
                    t.warn
                } else {
                    Style::new()
                },
            ),
        ]),
    ];
    for p in &info.stalled_paths {
        lines.push(Line::styled(format!("   {}", p.display()), t.dim));
    }
    if let Some((draw, read)) = ui {
        lines.push(plain(format!("ui draw {}  read {}", dur(draw), dur(read))));
    }
    lines
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use ratatui::crossterm::event::KeyCode;

    use super::*;
    use crate::app::keys::tests::press;
    use crate::tests::{render, render_buffer, style_at};

    /// Counters of a scan in progress: a known hold histogram (90 slices of about 200 µs, 9 of
    /// 1.5 ms, one of 3 ms) and one stalled directory.
    fn info(lost: u64) -> Info {
        let mut timing = Timing {
            slices: 100,
            locked_ns: 400_000_000,
            ..Timing::default()
        };
        timing.hold[Timing::bucket(Duration::from_micros(200))] = 90;
        timing.hold[Timing::bucket(Duration::from_micros(1500))] = 9;
        timing.hold[Timing::bucket(Duration::from_millis(3))] = 1;
        timing.wait[Timing::bucket(Duration::from_micros(30))] = 99;
        timing.wait[Timing::bucket(Duration::from_millis(5))] = 1;
        let budget = Budget::new(64 << 20);
        let got = budget.acquire(40 << 20);
        budget.release(got - (12 << 20));
        Info {
            stats: Stats {
                messages: 1_204_000,
                chunks: 12_000,
                closes: 9_810,
                entries: 1_900_000,
                discarded_job: 7,
                discarded_cut: 3,
                late: 2,
                stalled: 1,
                lost,
                ..Stats::default()
            },
            timing,
            counters: Counters {
                sent: 1_204_310,
                outstanding: (12, 1),
                queued: (3, 40),
            },
            budget: (budget.used(), budget.peak(), budget.capacity()),
            running: 1,
            stalled: 1,
            stalled_paths: vec![PathBuf::from("/home/jo/nfs")],
        }
    }

    fn open_app(info: Info, theme: Theme) -> App {
        let mut app = App::new(theme, Instant::now());
        toggle(&mut app);
        app.extras.debug = info;
        frame_drawn(
            &mut app,
            Duration::from_micros(1200),
            Duration::from_micros(310),
        );
        app
    }

    /// The text inside the panel's border, a line per row.
    fn panel_lines(frame: &str) -> Vec<String> {
        frame
            .lines()
            .filter_map(|l| l.split_once('│')?.1.rsplit_once('│'))
            .map(|(inside, _)| inside.trim().to_string())
            .collect()
    }

    #[test]
    fn debug_panel_shows_counters() {
        let mut app = open_app(info(0), Theme::no_color());
        let t0 = app.started;
        let out = render(&mut app, t0, 100, 20);
        insta::assert_snapshot!("debug_panel_mono", out);
        let panel = panel_lines(&out).join("\n");
        for want in [
            "sent 1.2M  received 1.2M  pending 310",
            "discarded job 7  cut 3  late 2",
            "corrupt 0  lost 0  duplicate 0",
            "transit 12.0 MiB / 64.0 MiB  peak 40.0 MiB",
            "tasks 12 (1 abandoned)  queued 3 + 40",
            // 1.9M entries in 400 ms of lock time.
            "lock 400.0ms  100 slices  4.8M entries/s",
            // Bucket bounds: 200 µs is in 192-224 µs, 1.5 ms in 1.28-1.54 ms, 3 ms in
            // 2.56-3.07 ms.
            "hold p50 <224µs  p99 <1.5ms  max <3.1ms",
            "over 2ms 1  wait p99 <32µs  max <5.1ms",
            "stalled dirs 1",
            "/home/jo/nfs",
            "ui draw 1.2ms  read 310µs",
        ] {
            assert!(panel.contains(want), "{want:?} is not in\n{out}");
        }
    }

    #[test]
    fn anomalies_are_marked_without_colour() {
        let mut app = open_app(info(2), Theme::no_color());
        let t0 = app.started;
        let out = render(&mut app, t0, 100, 20);
        assert!(out.contains("corrupt 0  lost 2!  duplicate 0"), "{out}");
        let colour = Theme::ansi16();
        let mut app = open_app(info(2), colour.clone());
        let buf = render_buffer(&mut app, t0, 100, 20);
        assert_eq!(style_at(&buf, "lost 2!").fg, colour.error.fg);
        assert_ne!(style_at(&buf, "corrupt 0").fg, colour.error.fg);
    }

    #[test]
    fn summary_has_the_same_numbers() {
        let info = info(2);
        let mut app = open_app(info.clone(), Theme::no_color());
        let t0 = app.started;
        let mut panel = panel_lines(&render(&mut app, t0, 100, 20));
        assert_eq!(panel.pop().as_deref(), Some("ui draw 1.2ms  read 310µs"));
        let summary = text(&info);
        assert_eq!(summary.lines().collect::<Vec<_>>(), panel, "{summary}");
        assert!(summary.contains("pending 310\n"), "{summary}");
    }

    #[test]
    fn panel_stays_left_of_the_details_pane() {
        let mut app = open_app(info(0), Theme::no_color());
        let t0 = app.started;
        crate::details::toggle(&mut app);
        let buf = render_buffer(&mut app, t0, 120, 20);
        let pane = app.areas.details.expect("details pane");
        let out = crate::tests::text(&buf);
        assert_eq!(buf[(pane.x - 1, pane.y)].symbol(), "╮", "{out}");
        assert_eq!(buf[(pane.x, pane.y)].symbol(), "╭", "{out}");
    }

    #[test]
    fn panel_is_hidden_on_a_small_terminal_and_keys_still_work() {
        let mut app = open_app(info(0), Theme::no_color());
        let t0 = app.started;
        let small = render(&mut app, t0, 49, 20);
        assert!(!small.contains("Debug"), "{small}");
        let low = render(&mut app, t0, 100, 15);
        assert!(!low.contains("Debug"), "{low}");
        // Not modal: the list keeps its keys while it is open, and F12 closes it.
        let fx = app.key(press(KeyCode::Char('n')), t0);
        assert!(fx.is_empty());
        let sorted = render(&mut app, t0, 100, 20);
        assert!(
            sorted.contains("sort: name") && sorted.contains("Debug"),
            "{sorted}"
        );
        app.key(press(KeyCode::F(12)), t0);
        assert!(!app.debug.open);
        let fits = render(&mut app, t0, 100, 20);
        assert!(!fits.contains("Debug"), "{fits}");
    }
}

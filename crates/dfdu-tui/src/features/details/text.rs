//! The pane's text: what it says about the entry [`super::read`] found, wrapped to the pane.

use dfdu_tree::{Kind, date_time};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::{BAR, Facts, Largest, Matched};
use crate::app::App;
use crate::format::{bar, count, human, plural, ratio};
use crate::model::snapshot::Mark;
use crate::theme::Theme;

/// The pane's lines being built, each wrapped to the inner width.
struct Text {
    out: Vec<Line<'static>>,
    width: usize,
}

impl Text {
    /// `s` in `style`, wrapped at spaces, each line indented by one column.
    fn add(&mut self, s: &str, style: Style) {
        for piece in wrap(s, self.width.saturating_sub(1)) {
            self.out.push(Line::styled(format!(" {piece}"), style));
        }
    }

    fn push(&mut self, line: Line<'static>) {
        self.out.push(line);
    }
}

/// The pane's lines for an inner width of `width`, most wanted first: where it is, what it is,
/// its sizes and problems, the largest children, then identity and boundaries.
pub(super) fn lines(app: &App, width: usize) -> Vec<Line<'static>> {
    let t = &app.theme;
    let mut out = Text {
        out: Vec::new(),
        width,
    };
    let Some(f) = &app.extras.details.facts else {
        out.add(&unread_note(app), t.fg_dim);
        return out.out;
    };
    out.add(&f.path.to_string(), t.fg_dim);
    kind(&mut out, t, f, width);
    if matches!(f.mark, Mark::OtherFs | Mark::Network) {
        let (info, hint) = crate::model::outside::pane_lines(app, &f.path);
        if let Some(i) = info {
            out.add(&i, Style::new());
        }
        out.add(&hint, t.fg_dim);
    }
    sizes(&mut out, app, f);
    if let Some(c) = &f.matched {
        rule_match(&mut out, t, c);
    }
    // A match counts as one category; the breakdown is for directories with matches below.
    if !f.categories.is_empty() && f.matched.is_none() {
        out.add("by category", t.title);
        for &(name, slot, size, n) in &f.categories {
            out.push(Line::from(vec![
                Span::raw(format!(" {:>10}  ", human(size))),
                Span::styled(name.to_string(), t.cat[slot]),
                Span::styled(format!(" · {n}"), t.fg_dim),
            ]));
        }
    }
    largest(&mut out, t, f);
    identity(&mut out, t, f);
    out.out
}

/// What the pane says when it has no facts: the row is not an entry of its own, or nothing is
/// selected.
fn unread_note(app: &App) -> String {
    let listed = app
        .selected_entry()
        .filter(|_| !crate::features::cleanup::active(app));
    match listed {
        Some(e) if e.ghost => "deleted".to_string(),
        Some(e) if e.pseudo => format!(
            "{} of data that entries of this directory share, counted here once. Not an \
             entry: it cannot be opened or deleted.",
            human(e.size)
        ),
        _ => "nothing selected".to_string(),
    }
}

/// What it is and, for a directory, its scan state: on one line when they fit.
fn kind(out: &mut Text, t: &Theme, f: &Facts, width: usize) {
    let plain = Style::new();
    if !f.is_dir {
        out.add(kind_name(f.kind), plain);
        return;
    }
    let style = match f.mark {
        Mark::None => plain,
        Mark::ErrorsInside => t.fg_dim,
        Mark::Scanning | Mark::Rescanning => t.scan_active,
        Mark::Error => t.status_error,
        _ => t.status_warn,
    };
    let head = format!(" {} · ", kind_name(f.kind));
    let s = state(f.mark);
    if head.chars().count() + s.chars().count() <= width {
        out.push(Line::from(vec![Span::raw(head), Span::styled(s, style)]));
    } else {
        out.add(kind_name(f.kind), plain);
        out.add(s, style);
    }
}

/// Its sizes, what grew since the last recorded scan, and its problems.
fn sizes(out: &mut Text, app: &App, f: &Facts) {
    let t = &app.theme;
    let plain = Style::new();
    let mut disk = format!("{} on disk", human(f.total.alloc));
    if f.compressed {
        disk.push_str(" at most (compressed)");
    }
    if f.is_dir {
        disk.push_str(" · ");
        disk.push_str(&counted(f.total.items, "item"));
    }
    out.add(&disk, plain);
    let other = format!(
        "{} apparent · {} like du",
        human(f.total.apparent),
        human(f.total.referenced)
    );
    out.add(&other, plain);
    let size = app.size_mode.of(&f.total);
    if let Some((d, then, at)) = crate::model::growth::since(app, &f.path, size, f.mark) {
        let s = if d == 0 {
            format!("the same as on {}", dfdu_tree::day(at))
        } else {
            format!(
                "{} (was {})",
                crate::model::growth::describe(d, at),
                human(then)
            )
        };
        out.add(&s, if d > 0 { t.status_warn } else { t.fg_dim });
    }
    if f.total.shared_out > 0 {
        let s = format!(
            "{} also used outside the scan: deleting it frees less",
            human(f.total.shared_out)
        );
        out.add(&s, t.status_warn);
    }
    if let Some(p) = &f.problem {
        out.add(p, t.status_error);
    }
    if f.errors_below > 0 {
        let s = format!("{} below", counted(f.errors_below, "error"));
        out.add(&s, t.status_error);
    }
}

/// The rule it matched, or the match it is in, and what keeps it from being deleted.
fn rule_match(out: &mut Text, t: &Theme, c: &Matched) {
    let plain = Style::new();
    out.push(Line::from(vec![
        Span::raw(" "),
        Span::styled(c.category.to_string(), t.cat[c.slot]),
        Span::styled(format!(" · {}", c.safety.as_str()), plain),
    ]));
    out.add(&c.rule, t.title);
    if let Some(p) = &c.above {
        out.add(&format!("in {p}"), t.fg_dim);
    }
    out.add(&c.explanation, plain);
    out.add(&format!("lost: {}", c.lost), t.fg_dim);
    if let Some(d) = &c.degraded {
        out.add(d, t.status_warn);
    }
    for (flag, note) in [
        (
            dfdu_tree::Class::VCS_TRACKED,
            "version control tracks files in it: protected",
        ),
        (
            dfdu_tree::Class::IN_USE,
            "a program that uses it was running",
        ),
        (
            dfdu_tree::Class::VCS_UNKNOWN,
            "git could not tell whether it tracks files in it: not recommended",
        ),
    ] {
        if c.flags & flag != 0 {
            out.add(note, t.status_warn);
        }
    }
}

/// Its largest children, with bars relative to its own size.
fn largest(out: &mut Text, t: &Theme, f: &Facts) {
    match &f.largest {
        Largest::None => {}
        Largest::Skipped(n) => {
            let s = format!(
                "largest children: not computed for {} entries",
                count(*n as u64)
            );
            out.add(&s, t.fg_dim);
        }
        Largest::Listed(kids) if kids.is_empty() => {}
        Largest::Listed(kids) => {
            out.add(&format!("largest by {}", f.size_mode.label()), t.title);
            let total = f.size_mode.of(&f.total).max(1);
            for k in kids {
                let style = if k.is_dir { t.dir } else { Style::new() };
                out.push(Line::from(vec![
                    Span::raw(" "),
                    Span::styled(bar(ratio(k.size, total), BAR, t.bars), t.bar_fill),
                    Span::raw(format!(" {:>10}  ", human(k.size))),
                    Span::styled(
                        format!("{}{}", k.name, if k.is_dir { "/" } else { "" }),
                        style,
                    ),
                ]));
            }
        }
    }
}

/// When it changed, its identity, and the boundaries below it where deletion stops.
fn identity(out: &mut Text, t: &Theme, f: &Facts) {
    let plain = Style::new();
    out.add(&format!("modified {}", date_time(f.mtime)), plain);
    let id = format!("device {} · inode {}", f.dev_ino.0, f.dev_ino.1);
    out.add(&id, plain);
    if let Some(l) = &f.label {
        out.add(l, plain);
    }
    let (mounts, ro) = f.inside;
    if mounts + ro > 0 {
        let mut parts = Vec::new();
        if mounts > 0 {
            parts.push(counted(mounts, "mount"));
        }
        if ro > 0 {
            parts.push(counted(ro, "read-only subvolume"));
        }
        let s = format!("inside: {}; deletion stops at them", parts.join(", "));
        out.add(&s, t.status_warn);
    }
}

/// The scan state of a directory, by the rules of its mark in the list.
pub(super) fn state(mark: Mark) -> &'static str {
    match mark {
        Mark::Scanning => "scanning",
        Mark::Stalled => "stalled: not responding",
        Mark::Rescanning => "rescanning",
        Mark::Error => "incomplete",
        Mark::Incomplete => "incomplete: scan stopped, r to rescan",
        Mark::Deleting => "being deleted",
        Mark::OtherFs => "not scanned: other filesystem",
        Mark::DupMount => "not scanned: mounted again",
        Mark::Network => "not scanned: network filesystem",
        Mark::Kernel => "not scanned: kernel filesystem",
        Mark::ErrorsInside => "complete, errors below",
        Mark::None => "complete",
    }
}

fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::File => "file",
        Kind::Dir => "directory",
        Kind::Symlink => "symlink",
        Kind::Other => "other",
    }
}

/// `n` and `noun`, in the plural unless `n` is one: `1 item`, `1.2k items`.
fn counted(n: u64, noun: &str) -> String {
    format!("{} {noun}{}", count(n), plural(n))
}

/// `text` in lines of at most `width` characters, broken at spaces; a word longer than a line
/// (a path) is cut.
pub(super) fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    let mut line = String::new();
    let mut len = 0;
    for word in text.split(' ') {
        let mut word: Vec<char> = word.chars().collect();
        if len > 0 && len + 1 + word.len() <= width {
            line.push(' ');
            line.extend(&word);
            len += 1 + word.len();
            continue;
        }
        if len > 0 {
            out.push(std::mem::take(&mut line));
        }
        while word.len() > width {
            out.push(word.drain(..width).collect());
        }
        line = word.iter().collect();
        len = word.len();
    }
    out.push(line);
    out
}

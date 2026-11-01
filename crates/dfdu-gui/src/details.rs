//! The details panel (`i`), on the right: what the tree knows about the selected entry, as the
//! terminal's details pane says it. Where it is, what it is and its scan state, its sizes and
//! what grew since the last recorded scan, its problems, the rule it matched (explanation, what
//! is lost, what protects it), its matches below by category, its largest children, then its
//! identity and the boundaries where deletion stops. What is read and when is the session's
//! (`dfdu_session::details`).

use dfdu_session::SessionState;
use dfdu_session::details::{Facts, Largest, Matched};
use dfdu_session::format::{count, human, plural, ratio};
use dfdu_session::growth;
use dfdu_session::palette::{Category, Token};
use dfdu_session::snapshot::Mark;
use dfdu_tree::{ClassFlags, Kind, date_time};
use egui::{Label, RichText, ScrollArea, Sense, Ui, vec2};

use crate::theme::Colors;

/// Width of the bars of the largest children.
const BAR: f32 = 48.0;

pub fn show(ui: &mut Ui, s: &SessionState, colors: &Colors) {
    ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| match &s.extras.details.facts {
            Some(f) => facts(ui, s, f, colors),
            None => text(ui, colors.text(unread_note(s), Token::FgDim)),
        });
}

/// A wrapped line.
fn text(ui: &mut Ui, t: impl Into<RichText>) {
    ui.add(Label::new(t.into()).wrap());
}

/// A heading of the panel.
fn heading(ui: &mut Ui, colors: &Colors, t: &str) {
    ui.add_space(ui.spacing().item_spacing.y);
    text(ui, colors.text(t, Token::Title).strong());
}

fn facts(ui: &mut Ui, s: &SessionState, f: &Facts, colors: &Colors) {
    let title = format!("{}{}", f.name, if f.is_dir { "/" } else { "" });
    ui.heading(colors.text(title, Token::Title));
    text(ui, colors.text(f.path.to_string(), Token::FgDim));
    kind(ui, f, colors);
    sizes(ui, s, f, colors);
    if let Some(c) = &f.matched {
        rule_match(ui, c, colors);
    }
    // A match counts as one category; the breakdown is for directories with matches below.
    if !f.categories.is_empty() && f.matched.is_none() {
        heading(ui, colors, "By category");
        egui::Grid::new("dfdu-details-categories").show(ui, |ui| {
            for &(name, slot, size, n) in &f.categories {
                ui.label(human(size));
                ui.label(colors.text(name, category(slot)));
                ui.label(colors.text(count(n), Token::FgDim));
                ui.end_row();
            }
        });
    }
    largest(ui, f, colors);
    identity(ui, f, colors);
}

/// The colour of category slot `slot`.
fn category(slot: usize) -> Token {
    Category::ALL
        .get(slot)
        .map_or(Token::CatOther, |c| c.token())
}

/// What the panel says without facts: the row is not an entry of its own, or nothing is
/// selected.
fn unread_note(s: &SessionState) -> String {
    match s.selected_entry() {
        Some(e) if e.ghost => "deleted".to_string(),
        Some(e) if e.pseudo => format!(
            "{} of data that entries of this directory share, counted here once. Not an \
             entry: it cannot be opened or deleted.",
            human(e.size)
        ),
        _ => "nothing selected".to_string(),
    }
}

/// What it is and, for a directory, its scan state.
fn kind(ui: &mut Ui, f: &Facts, colors: &Colors) {
    if !f.is_dir {
        text(ui, kind_name(f.kind));
        return;
    }
    let token = match f.mark {
        Mark::None => Token::Fg,
        Mark::ErrorsInside => Token::FgDim,
        Mark::Scanning | Mark::Rescanning => Token::ScanActive,
        Mark::Error => Token::StatusError,
        _ => Token::StatusWarn,
    };
    ui.horizontal_wrapped(|ui| {
        ui.label(format!("{} ·", kind_name(f.kind)));
        ui.label(colors.text(state(f.mark), token));
    });
}

/// Its sizes, what grew since the last recorded scan, and its problems.
fn sizes(ui: &mut Ui, s: &SessionState, f: &Facts, colors: &Colors) {
    let mut disk = format!("{} on disk", human(f.total.alloc));
    if f.compressed {
        disk.push_str(" at most (compressed)");
    }
    if f.is_dir {
        disk.push_str(" · ");
        disk.push_str(&counted(f.total.items, "item"));
    }
    text(ui, disk);
    text(
        ui,
        format!(
            "{} apparent · {} like du",
            human(f.total.apparent),
            human(f.total.referenced)
        ),
    );
    let size = s.size_mode.of(&f.total);
    if let Some((d, then, at)) =
        growth::since(s.growth.as_deref(), s.size_mode, &f.path, size, f.mark)
    {
        let line = if d == 0 {
            format!("the same as on {}", dfdu_tree::day(at))
        } else {
            format!("{} (was {})", growth::describe(d, at), human(then))
        };
        let token = if d > 0 {
            Token::StatusWarn
        } else {
            Token::FgDim
        };
        text(ui, colors.text(line, token));
    }
    if f.total.shared_out > 0 {
        let line = format!(
            "{} also used outside the scan: deleting it frees less",
            human(f.total.shared_out)
        );
        text(ui, colors.text(line, Token::StatusWarn));
    }
    if let Some(p) = &f.problem {
        text(ui, colors.text(p, Token::StatusError));
    }
    if f.errors_below > 0 {
        let line = format!("{} below", counted(f.errors_below, "error"));
        text(ui, colors.text(line, Token::StatusError));
    }
}

/// The rule it matched, or the match it is in, and what keeps it from being deleted.
fn rule_match(ui: &mut Ui, c: &Matched, colors: &Colors) {
    heading(ui, colors, "Rule match");
    ui.horizontal_wrapped(|ui| {
        ui.label(colors.text(c.category, category(c.slot)));
        ui.label(format!("· {}", c.safety.as_str()));
    });
    text(ui, colors.text(&c.rule, Token::Title));
    if let Some(p) = &c.above {
        text(ui, colors.text(format!("in {p}"), Token::FgDim));
    }
    text(ui, c.explanation.as_str());
    text(ui, colors.text(format!("lost: {}", c.lost), Token::FgDim));
    if let Some(d) = &c.degraded {
        text(ui, colors.text(d, Token::StatusWarn));
    }
    for (flag, note) in [
        (
            ClassFlags::VCS_TRACKED,
            "version control tracks files in it: protected",
        ),
        (ClassFlags::IN_USE, "a program that uses it was running"),
        (
            ClassFlags::VCS_UNKNOWN,
            "git could not tell whether it tracks files in it: not recommended",
        ),
    ] {
        if c.flags.has(flag) {
            text(ui, colors.text(note, Token::StatusWarn));
        }
    }
}

/// Its largest children, with bars relative to its own size.
fn largest(ui: &mut Ui, f: &Facts, colors: &Colors) {
    match &f.largest {
        Largest::None => {}
        Largest::Skipped(n) => {
            let line = format!(
                "largest children: not computed for {} entries",
                count(*n as u64)
            );
            text(ui, colors.text(line, Token::FgDim));
        }
        Largest::Listed(kids) if kids.is_empty() => {}
        Largest::Listed(kids) => {
            heading(ui, colors, &format!("Largest by {}", f.size_mode.label()));
            let total = f.size_mode.of(&f.total).max(1);
            egui::Grid::new("dfdu-details-largest").show(ui, |ui| {
                for k in kids {
                    bar(ui, colors, ratio(k.size, total));
                    ui.label(human(k.size));
                    let name = format!("{}{}", k.name, if k.is_dir { "/" } else { "" });
                    let token = if k.is_dir { Token::Dir } else { Token::Fg };
                    ui.label(colors.text(name, token));
                    ui.end_row();
                }
            });
        }
    }
}

/// A bar of `frac` of [`BAR`].
fn bar(ui: &mut Ui, colors: &Colors, frac: f64) {
    let h = ui.text_style_height(&egui::TextStyle::Body) * 0.6;
    let (rect, _) = ui.allocate_exact_size(vec2(BAR, h), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 2.0, colors.fg(Token::BarEmpty));
    #[expect(clippy::cast_possible_truncation, reason = "a width on screen")]
    let w = rect.width() * frac.clamp(0.0, 1.0) as f32;
    let filled = egui::Rect::from_min_size(rect.min, vec2(w, rect.height()));
    p.rect_filled(filled, 2.0, colors.fg(Token::BarFill));
}

/// When it changed, its identity, and the boundaries below it where deletion stops.
fn identity(ui: &mut Ui, f: &Facts, colors: &Colors) {
    ui.add_space(ui.spacing().item_spacing.y);
    text(ui, format!("modified {}", date_time(f.mtime)));
    text(
        ui,
        format!("device {} · inode {}", f.dev_ino.0, f.dev_ino.1),
    );
    if let Some(l) = &f.label {
        text(ui, l.as_str());
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
        let line = format!("inside: {}; deletion stops at them", parts.join(", "));
        text(ui, colors.text(line, Token::StatusWarn));
    }
}

/// The scan state of a directory, by the rules of its mark in the list (as the terminal's
/// pane words it).
fn state(mark: Mark) -> &'static str {
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

//! The details panel (`i`), on the right: what the tree knows about the selected entry, as the
//! terminal's details pane says it. Where it is, what it is and its scan state, its sizes and
//! what grew since the last recorded scan, its problems, the rule it matched (explanation, what
//! is lost, what protects it), its matches below by category, its largest children, then its
//! identity and the boundaries where deletion stops. What is read and when is the session's
//! (`dfdu_session::details`).

use dfdu_engine::tilde;
use dfdu_session::SessionState;
use dfdu_session::actions::Action;
use dfdu_session::details::{Facts, Largest, Matched};
use dfdu_session::format::{count, human, plural, ratio};
use dfdu_session::growth;
use dfdu_session::palette::Token;
use dfdu_session::snapshot::Mark;
use dfdu_tree::{ClassFlags, Kind, Safety, date_time};
use egui::{Label, RichText, ScrollArea, Ui, vec2};

use crate::browser::Intent;
use crate::kit::{self, Kind as ButtonKind};
use crate::list::category;
use crate::theme::Colors;

pub fn show(ui: &mut Ui, s: &SessionState, colors: &Colors, out: &mut Vec<Intent>) {
    ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        match &s.extras.details.facts {
            Some(f) => facts(ui, s, f, colors, out),
            None => text(
                ui,
                kit::sans(unread_note(s), 12.0).color(colors.fg(Token::FgDim)),
            ),
        }
    });
}

/// A wrapped line.
fn text(ui: &mut Ui, t: impl Into<RichText>) {
    ui.add(Label::new(t.into()).wrap());
}

/// A wrapped line of copy, 12 on 18, in `token`.
fn copy(ui: &mut Ui, colors: &Colors, t: impl Into<String>, token: Token) {
    text(ui, kit::sans(t, 12.0).color(colors.fg(token)));
}

/// A section: a hairline in `divider`, then its heading.
fn heading(ui: &mut Ui, colors: &Colors, t: &str) {
    kit::rule(ui, colors.chrome.divider, 6.0);
    text(ui, kit::title(t, 13.0).color(colors.fg(Token::Title)));
}

/// The safety glyph and colour of a level.
pub fn safety(level: Safety) -> (&'static str, Token) {
    match level {
        Safety::Safe => ("✓", Token::SafetySafe),
        Safety::Regenerable => ("↻", Token::SafetySafe),
        Safety::Review => ("?", Token::SafetyReview),
        Safety::Manual => ("⚒", Token::SafetyKeep),
        Safety::Protected => ("⊘", Token::SafetyKeep),
    }
}

fn facts(ui: &mut Ui, s: &SessionState, f: &Facts, colors: &Colors, out: &mut Vec<Intent>) {
    let title = format!("{}{}", f.name, if f.is_dir { "/" } else { "" });
    text(
        ui,
        kit::mono_bold(title, 20.0).color(colors.fg(Token::Title)),
    );
    text(
        ui,
        kit::mono(tilde(&f.path, s.home.as_deref()), 11.0).color(colors.fg(Token::FgDim)),
    );
    total(ui, s, f, colors);
    grid(ui, s, f, colors);
    notes(ui, s, f, colors);
    // A match counts as one category; the breakdown is for directories with matches below.
    if !f.categories.is_empty() && f.matched.is_none() {
        heading(ui, colors, "By category");
        egui::Grid::new("dfdu-details-categories")
            .spacing(vec2(12.0, 4.0))
            .show(ui, |ui| {
                for &(name, slot, size, n) in &f.categories {
                    kit::chip(ui, name, colors.fg(category(slot)));
                    ui.label(kit::mono(human(size), 12.0));
                    ui.label(kit::mono(count(n), 12.0).color(colors.fg(Token::FgDim)));
                    ui.end_row();
                }
            });
    }
    largest(ui, f, colors);
    if let Some(c) = &f.matched {
        rule_match(ui, c, colors);
    }
    identity(ui, f, colors);
    actions(ui, s, colors, out);
}

/// The big total, between rules in `border`, with its share of the scan.
fn total(ui: &mut Ui, s: &SessionState, f: &Facts, colors: &Colors) {
    let border = colors.fg(Token::Border);
    kit::rule(ui, border, 2.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        let size = s.size_mode.of(&f.total);
        ui.label(kit::mono_bold(human(size), 32.0).color(colors.fg(Token::Header)));
        let root = s.size_mode.of(&s.snap.root_total).max(1);
        if f.path != s.snap.root_path {
            let of = tilde(&s.snap.root_path, s.home.as_deref());
            let share = format!("{:.1}% of {of}", ratio(size, root) * 100.0);
            ui.label(kit::sans(share, 12.0).color(colors.fg(Token::FgDim)));
        }
    });
    kit::rule(ui, border, 2.0);
}

/// The facts as a grid of names and values.
fn grid(ui: &mut Ui, s: &SessionState, f: &Facts, colors: &Colors) {
    let dim = colors.fg(Token::FgDim);
    let key = |ui: &mut Ui, k: &str| {
        ui.label(kit::sans(k, 12.0).color(dim));
    };
    let value = |ui: &mut Ui, v: String| {
        ui.label(kit::mono(v, 12.0));
    };
    egui::Grid::new("dfdu-details-facts")
        .spacing(vec2(12.0, 4.0))
        .show(ui, |ui| {
            key(ui, "Kind");
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(kit::sans(kind_name(f.kind), 12.0));
                if f.is_dir {
                    let token = match f.mark {
                        Mark::None => Token::FgDim,
                        Mark::ErrorsInside => Token::FgDim,
                        Mark::Scanning | Mark::Rescanning => Token::ScanActive,
                        Mark::Error => Token::StatusError,
                        _ => Token::StatusWarn,
                    };
                    ui.label(
                        kit::sans(format!("· {}", state(f.mark)), 12.0).color(colors.fg(token)),
                    );
                }
            });
            ui.end_row();
            let (other, other_name) = match s.size_mode {
                dfdu_engine::SizeMode::Disk => (f.total.apparent, "Apparent"),
                _ => (f.total.alloc, "On disk"),
            };
            key(ui, other_name);
            let mut v = human(other);
            if f.compressed {
                v.push_str(" (compressed)");
            }
            value(ui, v);
            ui.end_row();
            key(ui, "Like du");
            value(ui, human(f.total.referenced));
            ui.end_row();
            if f.is_dir {
                key(ui, "Items");
                value(ui, count(f.total.items));
                ui.end_row();
            }
            key(ui, "Modified");
            value(ui, date_time(f.mtime));
            ui.end_row();
            if let Some(c) = &f.matched {
                key(ui, "Category");
                kit::chip(ui, c.category, colors.fg(category(c.slot)));
                ui.end_row();
                key(ui, "Rule");
                ui.add(Label::new(kit::mono(&c.rule, 12.0)).wrap());
                ui.end_row();
            }
        });
}

/// What grew since the last recorded scan, what is shared outside, and the problems.
fn notes(ui: &mut Ui, s: &SessionState, f: &Facts, colors: &Colors) {
    let size = s.size_mode.of(&f.total);
    if let Some((d, then, at)) =
        growth::since(s.growth.as_deref(), s.size_mode, &f.path, size, f.mark)
    {
        let line = if d == 0 {
            format!("The same as on {}", dfdu_tree::day(at))
        } else {
            format!("{} (was {})", growth::describe(d, at), human(then))
        };
        let token = if d > 0 {
            Token::StatusWarn
        } else {
            Token::FgDim
        };
        copy(ui, colors, line, token);
    }
    if f.total.shared_out > 0 {
        let line = format!(
            "{} also used outside the scan: deleting it frees less",
            human(f.total.shared_out)
        );
        copy(ui, colors, line, Token::StatusWarn);
    }
    if let Some(p) = &f.problem {
        copy(ui, colors, p.as_str(), Token::StatusError);
    }
    if f.errors_below > 0 {
        let line = format!("{} below", counted(f.errors_below, "error"));
        copy(ui, colors, line, Token::StatusError);
    }
}

/// Reveal in the file manager and copy the path.
fn actions(ui: &mut Ui, s: &SessionState, colors: &Colors, out: &mut Vec<Intent>) {
    kit::rule(ui, colors.chrome.divider, 6.0);
    ui.horizontal_wrapped(|ui| {
        if s.outside.local {
            let reveal = if cfg!(target_os = "macos") {
                "Reveal in Finder"
            } else if cfg!(windows) {
                "Show in Explorer"
            } else {
                "Open folder"
            };
            if kit::button(ui, colors, ButtonKind::Secondary, reveal, None, true).clicked() {
                out.push(Intent::Act(Action::OpenPath));
            }
        }
        if kit::button(ui, colors, ButtonKind::Secondary, "Copy path", None, true).clicked() {
            out.push(Intent::Act(Action::CopyPath));
        }
    });
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

/// What deleting the match loses (lost before gained), its safety as a badge, and what keeps
/// it from being deleted.
fn rule_match(ui: &mut Ui, c: &Matched, colors: &Colors) {
    heading(ui, colors, "What is lost");
    if let Some(p) = &c.above {
        copy(ui, colors, format!("Inside the match {p}"), Token::FgDim);
    }
    copy(ui, colors, c.explanation.as_str(), Token::Fg);
    copy(ui, colors, c.lost.as_str(), Token::FgDim);
    if let Some(d) = &c.degraded {
        copy(ui, colors, d.as_str(), Token::StatusWarn);
    }
    for (flag, note) in [
        (
            ClassFlags::VCS_TRACKED,
            "Version control tracks files in it: protected.",
        ),
        (ClassFlags::IN_USE, "A program that uses it was running."),
        (
            ClassFlags::VCS_UNKNOWN,
            "Git could not tell whether it tracks files in it: not recommended.",
        ),
    ] {
        if c.flags.has(flag) {
            copy(ui, colors, note, Token::StatusWarn);
        }
    }
    let (glyph, token) = safety(c.safety);
    let name = crate::cleanup::section_name(c.safety).0;
    kit::badge(ui, &format!("{glyph} {name}"), colors.fg(token));
}

/// Its largest children, with bars relative to its own size.
fn largest(ui: &mut Ui, f: &Facts, colors: &Colors) {
    match &f.largest {
        Largest::None => {}
        Largest::Skipped(n) => {
            heading(ui, colors, "Largest inside");
            let line = format!("Not computed for {} entries.", count(*n as u64));
            copy(ui, colors, line, Token::FgDim);
        }
        Largest::Listed(kids) if kids.is_empty() => {}
        Largest::Listed(kids) => {
            heading(ui, colors, "Largest inside");
            let total = f.size_mode.of(&f.total).max(1);
            for k in kids {
                let name = format!("{}{}", k.name, if k.is_dir { "/" } else { "" });
                let t = if k.is_dir {
                    kit::mono_bold(name, 12.0).color(colors.fg(Token::Dir))
                } else {
                    kit::mono(name, 12.0)
                };
                ui.horizontal(|ui| {
                    let size_w = 76.0;
                    let bar_w = 64.0;
                    let name_w = (ui.available_width() - size_w - bar_w - 16.0).max(40.0);
                    ui.allocate_ui_with_layout(
                        vec2(name_w, 18.0),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.set_min_width(name_w);
                            ui.add(Label::new(t).truncate());
                        },
                    );
                    kit::size_bar(ui, colors, bar_w, 6.0, ratio(k.size, total));
                    ui.allocate_ui_with_layout(
                        vec2(size_w, 18.0),
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            ui.label(kit::mono(human(k.size), 12.0));
                        },
                    );
                });
            }
        }
    }
}

/// Its identity, and the boundaries below it where deletion stops.
fn identity(ui: &mut Ui, f: &Facts, colors: &Colors) {
    kit::rule(ui, colors.chrome.divider, 6.0);
    let dim = |ui: &mut Ui, t: String| {
        text(ui, kit::mono(t, 11.0).color(colors.fg(Token::FgDim)));
    };
    dim(
        ui,
        format!("device {} · inode {}", f.dev_ino.0, f.dev_ino.1),
    );
    if let Some(l) = &f.label {
        dim(ui, l.clone());
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
        let line = format!("Inside: {}; deletion stops at them.", parts.join(", "));
        copy(ui, colors, line, Token::StatusWarn);
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

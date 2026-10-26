//! The disk picker (`dfdu --disks`): the mounted filesystems with their usage; the chosen one
//! is scanned.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use dfdu_sys::{Disk, FsClass};
use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::theme::{Theme, bar, human};

/// How long each filesystem gets to report its size.
pub const DISK_TIMEOUT: Duration = Duration::from_secs(2);

pub struct Picker {
    pub disks: Vec<Disk>,
    pub cursor: usize,
    pub status: Option<String>,
}

pub enum Picked {
    Scan(PathBuf),
    Quit,
}

impl Picker {
    pub fn new(disks: Vec<Disk>) -> Self {
        Picker {
            disks,
            cursor: 0,
            status: None,
        }
    }

    /// Handles one key; `Some` when the picker is done.
    pub fn key(&mut self, code: KeyCode) -> Option<Picked> {
        let n = self.disks.len();
        self.status = None;
        match code {
            KeyCode::Up | KeyCode::Char('k') => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.cursor = (self.cursor + 1).min(n.saturating_sub(1))
            }
            KeyCode::Home | KeyCode::Char('g') => self.cursor = 0,
            KeyCode::End | KeyCode::Char('G') => self.cursor = n.saturating_sub(1),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                let d = self.disks.get(self.cursor)?;
                if d.space.is_none() {
                    self.status = Some(format!(
                        "{} is not responding; not scanning it",
                        d.mount_point.display()
                    ));
                    return None;
                }
                return Some(Picked::Scan(d.mount_point.clone()));
            }
            KeyCode::Char('q') | KeyCode::Esc => return Some(Picked::Quit),
            _ => {}
        }
        None
    }
}

pub fn draw(f: &mut Frame, p: &Picker, t: &Theme) {
    let [header, list, status, footer] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(f.area());
    f.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled(" dfdu ", t.title),
                Span::styled(" choose a disk to scan", t.title),
            ]),
            Line::styled(
                format!(
                    " {:<24} {:<8} {:>10} {:>10} {:>10}",
                    "mounted on", "type", "size", "used", "free"
                ),
                t.dim,
            ),
        ]),
        header,
    );
    let height = list.height as usize;
    let top = p.cursor.saturating_sub(height.saturating_sub(1));
    let bw = (list.width as usize).saturating_sub(80).clamp(0, 20);
    let lines: Vec<Line> = p
        .disks
        .iter()
        .enumerate()
        .skip(top)
        .take(height)
        .map(|(i, d)| row(d, i == p.cursor, bw, t))
        .collect();
    if lines.is_empty() {
        f.render_widget(
            Paragraph::new(Line::styled(" No disks found.", t.dim)),
            list,
        );
    } else {
        f.render_widget(Paragraph::new(lines), list);
    }
    if let Some(s) = &p.status {
        f.render_widget(
            Paragraph::new(Line::styled(format!(" {s}"), t.warn)),
            status,
        );
    }
    f.render_widget(
        Paragraph::new(Line::styled(" ↑↓ move  Enter scan  q quit", t.dim)),
        footer,
    );
}

fn row<'a>(d: &Disk, selected: bool, bw: usize, t: &Theme) -> Line<'a> {
    let mut name = d.mount_point.display().to_string();
    if name.chars().count() > 24 {
        let skip = name.chars().count() - 23;
        name = format!("…{}", name.chars().skip(skip).collect::<String>());
    }
    let style = if selected { t.selected } else { t.dir };
    let mut spans = vec![
        Span::styled(format!(" {name:<24}"), style),
        Span::raw(format!(" {:<8}", d.fstype)),
    ];
    match d.space {
        Some(s) => {
            spans.push(Span::raw(format!(
                " {:>10} {:>10} {:>10}",
                human(s.total),
                human(s.used),
                human(s.available)
            )));
            let frac = s.used as f64 / s.total.max(1) as f64;
            if bw > 0 {
                spans.push(Span::raw("  "));
                spans.push(Span::styled(bar(frac, bw), t.bar));
            }
            spans.push(Span::raw(format!("  {:>3.0}%", frac * 100.0)));
        }
        None => spans.push(Span::styled("  not responding", t.warn)),
    }
    if d.class == FsClass::Network {
        spans.push(Span::styled("  network", t.dim));
    }
    if !d.also_at.is_empty() {
        let also: Vec<String> = d.also_at.iter().map(|p| p.display().to_string()).collect();
        spans.push(Span::styled(
            format!("  also at {}", also.join(", ")),
            t.dim,
        ));
    }
    Line::from(spans)
}

/// Runs the picker on an initialised terminal until a disk is chosen or the user quits.
pub fn pick(terminal: &mut ratatui::DefaultTerminal, theme: &Theme) -> io::Result<Picked> {
    let mut p = Picker::new(dfdu_sys::disks(DISK_TIMEOUT)?);
    loop {
        terminal.draw(|f| draw(f, &p, theme))?;
        if let Event::Key(k) = event::read()? {
            if k.kind == KeyEventKind::Release {
                continue;
            }
            if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
                return Ok(Picked::Quit);
            }
            if let Some(done) = p.key(k.code) {
                return Ok(done);
            }
        }
    }
}

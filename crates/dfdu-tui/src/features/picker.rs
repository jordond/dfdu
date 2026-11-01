//! The disk picker (`dfdu --disks`): the mounted filesystems with their usage; the chosen one
//! is scanned. A macOS data volume comes first, chosen to begin with: the system volume shows
//! its files only through firmlinks it does not descend.

use std::io;
use std::path::PathBuf;

use dfdu_sys::{Disk, FsClass, Role};
use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::format::{bar, human, ratio};
use crate::theme::Theme;

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
    #[must_use]
    pub fn new(mut disks: Vec<Disk>) -> Self {
        disks.sort_by_key(|d| d.role != Some(Role::Data));
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
                self.cursor = (self.cursor + 1).min(n.saturating_sub(1));
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
                t.fg_dim,
            ),
        ]),
        header,
    );
    let height = list.height as usize;
    let top = p.cursor.saturating_sub(height.saturating_sub(1));
    let bw = (list.width as usize).saturating_sub(80).min(20);
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
            Paragraph::new(Line::styled(" No disks found.", t.fg_dim)),
            list,
        );
    } else {
        f.render_widget(Paragraph::new(lines), list);
    }
    if let Some(s) = &p.status {
        f.render_widget(
            Paragraph::new(Line::styled(format!(" {s}"), t.status_warn)),
            status,
        );
    }
    f.render_widget(
        Paragraph::new(Line::styled(" ↑↓ move  Enter scan  q quit", t.fg_dim)),
        footer,
    );
}

fn row<'a>(d: &Disk, selected: bool, bw: usize, t: &Theme) -> Line<'a> {
    let mut name = d.mount_point.display().to_string();
    if name.chars().count() > 24 {
        let skip = name.chars().count() - 23;
        name = format!("…{}", name.chars().skip(skip).collect::<String>());
    }
    let style = if selected { t.cursor } else { t.dir };
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
            let frac = ratio(s.used, s.total.max(1));
            if bw > 0 {
                spans.push(Span::raw("  "));
                spans.push(Span::styled(bar(frac, bw, t.bars), t.bar_fill));
            }
            spans.push(Span::raw(format!("  {:>3.0}%", frac * 100.0)));
            if let Some(p) = s.purgeable.filter(|&p| p > 0) {
                spans.push(Span::styled(
                    format!("  (+{} purgeable)", human(p)),
                    t.fg_dim,
                ));
            }
        }
        None => spans.push(Span::styled("  not responding", t.status_warn)),
    }
    if d.class == FsClass::Network {
        spans.push(Span::styled("  network", t.fg_dim));
    }
    if let Some(role) = d.role {
        spans.push(Span::styled(format!("  {}", role.label()), t.fg_dim));
    }
    if !d.also_at.is_empty() {
        let also: Vec<String> = d.also_at.iter().map(|p| p.display().to_string()).collect();
        spans.push(Span::styled(
            format!("  also at {}", also.join(", ")),
            t.fg_dim,
        ));
    }
    if !d.shares_space_with.is_empty() {
        let with: Vec<String> = d
            .shares_space_with
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        spans.push(Span::styled(
            format!("  shares space with {}", with.join(", ")),
            t.fg_dim,
        ));
    }
    Line::from(spans)
}

/// Runs the picker on an initialised terminal until a disk is chosen or the user quits.
pub fn pick(terminal: &mut ratatui::DefaultTerminal, theme: &Theme) -> io::Result<Picked> {
    let mut p = Picker::new(dfdu_sys::disks(dfdu_sys::DISK_TIMEOUT)?);
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

#[cfg(test)]
mod tests {
    use dfdu_sys::{Disk, FsClass, Role, Space};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::KeyCode;

    use super::{Picked, Picker, draw};
    use crate::theme::Theme;

    fn apfs(at: &str, role: Option<Role>, with: &str) -> Disk {
        let gib = 1u64 << 30;
        Disk {
            mount_point: at.into(),
            also_at: vec![],
            fstype: "apfs".into(),
            source: String::new(),
            label: None,
            class: FsClass::Local,
            space: Some(Space {
                total: 926 * gib,
                used: 822 * gib,
                available: 104 * gib,
                purgeable: Some(30 * gib),
            }),
            role,
            shares_space_with: vec![with.into()],
        }
    }

    /// The system volume and its data volume, as `dfdu_sys::disks` lists them on macOS: the
    /// data volume comes first and is chosen to begin with.
    #[test]
    fn data_volume_first() {
        let mut p = Picker::new(vec![
            apfs("/", Some(Role::System), "/System/Volumes/Data"),
            apfs("/System/Volumes/Data", Some(Role::Data), "/"),
        ]);
        let theme = Theme::no_color();
        let mut term = Terminal::new(TestBackend::new(160, 6)).expect("terminal");
        term.draw(|f| draw(f, &p, &theme)).expect("draw");
        let out = crate::tests::text(term.backend().buffer());
        insta::assert_snapshot!("data_volume_first", out);
        match p.key(KeyCode::Enter) {
            Some(Picked::Scan(path)) => {
                assert_eq!(path, std::path::Path::new("/System/Volumes/Data"));
            }
            _ => panic!("expected a scan of the data volume"),
        }
    }
}

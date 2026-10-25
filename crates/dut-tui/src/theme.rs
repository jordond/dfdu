//! Semantic styles. Defaults use only the 16 ANSI colours so they read on light and dark
//! backgrounds; `NO_COLOR` keeps the modifiers (bold, reverse) and drops every colour. Theme
//! files and truecolor palettes come in M3.

use ratatui::style::{Color, Modifier, Style};

#[derive(Clone, Debug)]
pub struct Theme {
    pub title: Style,
    pub dim: Style,
    pub bar: Style,
    pub dir: Style,
    pub selected: Style,
    pub warn: Style,
    pub error: Style,
    pub key: Style,
}

impl Theme {
    pub fn ansi16() -> Self {
        Theme {
            title: Style::new().add_modifier(Modifier::BOLD),
            dim: Style::new().fg(Color::DarkGray),
            bar: Style::new().fg(Color::Cyan),
            dir: Style::new().fg(Color::Blue).add_modifier(Modifier::BOLD),
            selected: Style::new().add_modifier(Modifier::REVERSED),
            warn: Style::new().fg(Color::Yellow),
            error: Style::new().fg(Color::Red),
            key: Style::new().add_modifier(Modifier::BOLD),
        }
    }

    pub fn no_color() -> Self {
        Theme {
            title: Style::new().add_modifier(Modifier::BOLD),
            dim: Style::new(),
            bar: Style::new(),
            dir: Style::new().add_modifier(Modifier::BOLD),
            selected: Style::new().add_modifier(Modifier::REVERSED),
            warn: Style::new().add_modifier(Modifier::BOLD),
            error: Style::new().add_modifier(Modifier::BOLD),
            key: Style::new().add_modifier(Modifier::BOLD),
        }
    }

    /// `NO_COLOR` set to any non-empty value turns colour off (no-color.org).
    pub fn from_env() -> Self {
        match std::env::var_os("NO_COLOR") {
            Some(v) if !v.is_empty() => Self::no_color(),
            _ => Self::ansi16(),
        }
    }
}

/// Human-readable size with binary units.
pub fn human(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

/// Compact count: 812, 1.2k, 3.4M.
pub fn count(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => format!("{:.1}k", n as f64 / 1e3),
        1_000_000..1_000_000_000 => format!("{:.1}M", n as f64 / 1e6),
        _ => format!("{:.1}G", n as f64 / 1e9),
    }
}

/// A bar of `width` cells filled to `frac` (0..=1), with eighth-block resolution.
pub fn bar(frac: f64, width: usize) -> String {
    const PARTS: [char; 8] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉'];
    let eighths = (frac.clamp(0.0, 1.0) * (width * 8) as f64).round() as usize;
    let mut s = String::with_capacity(width * 3);
    for _ in 0..eighths / 8 {
        s.push('█');
    }
    if eighths / 8 < width {
        s.push(PARTS[eighths % 8]);
        for _ in eighths / 8 + 1..width {
            s.push(' ');
        }
    }
    s
}

/// Braille spinner frame for directories still being scanned.
pub fn spinner(frame: u64) -> char {
    const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    FRAMES[(frame % FRAMES.len() as u64) as usize]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bars_have_fixed_width() {
        for f in [0.0, 0.01, 0.5, 0.999, 1.0] {
            assert_eq!(bar(f, 10).chars().count(), 10, "frac {f}");
        }
        assert_eq!(bar(1.0, 4), "████");
        assert_eq!(bar(0.5, 4), "██  ");
        assert_eq!(bar(0.0625, 4), "▎   ");
    }

    #[test]
    fn formats() {
        assert_eq!(human(512), "512 B");
        assert_eq!(human(1536), "1.5 KiB");
        assert_eq!(count(812), "812");
        assert_eq!(count(1_200), "1.2k");
    }
}

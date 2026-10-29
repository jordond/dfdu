//! Text for numbers and names: compact counts, bars, the spinner, names shortened to fit.

use unicode_width::UnicodeWidthChar;

/// `part` as a fraction of `whole`, for bars and percentages.
#[must_use]
#[expect(
    clippy::cast_precision_loss,
    reason = "a fraction drawn as a bar or a percentage needs a few significant digits, not 64 bits"
)]
pub fn ratio(part: u64, whole: u64) -> f64 {
    part as f64 / whole as f64
}

/// `part` as a percentage of `whole`.
#[must_use]
#[expect(
    clippy::cast_precision_loss,
    reason = "a percentage is shown with one decimal, which needs a few significant digits"
)]
pub fn percent(part: u64, whole: u64) -> f64 {
    part as f64 * 100.0 / whole as f64
}

/// Compact count: 812, 1.2k, 3.4M.
#[must_use]
pub fn count(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => format!("{:.1}k", ratio(n, 1_000)),
        1_000_000..1_000_000_000 => format!("{:.1}M", ratio(n, 1_000_000)),
        _ => format!("{:.1}G", ratio(n, 1_000_000_000)),
    }
}

/// How bars are drawn: `--bars` and the theme key `bars`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Bars {
    /// Eighth blocks (`█▌`).
    #[default]
    Unicode,
    /// `#` for terminals and fonts without block characters; treemap frames in `=`, `|` and `+`.
    Ascii,
}

impl Bars {
    /// The names [`Bars::parse`] accepts.
    pub const NAMES: [&'static str; 2] = ["unicode", "ascii"];

    /// `unicode` or `ascii`.
    #[must_use]
    pub fn parse(s: &str) -> Option<Bars> {
        match s {
            "unicode" => Some(Bars::Unicode),
            "ascii" => Some(Bars::Ascii),
            _ => None,
        }
    }
}

/// A bar of `width` cells filled to `frac` (0..=1): with eighth-block resolution, or in whole
/// cells of `#` for [`Bars::Ascii`].
#[must_use]
pub fn bar(frac: f64, width: usize, bars: Bars) -> String {
    const PARTS: [char; 8] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉'];
    let eighths = eighths(frac, width);
    if bars == Bars::Ascii {
        let full = (eighths + 4) / 8;
        return format!("{}{}", "#".repeat(full), " ".repeat(width - full));
    }
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

/// How many eighths of `width` cells `frac` fills, rounded.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "frac is clamped to 0..=1, so the result is between 0 and width * 8"
)]
fn eighths(frac: f64, width: usize) -> usize {
    (frac.clamp(0.0, 1.0) * (width * 8) as f64).round() as usize
}

/// Braille spinner frame for directories still being scanned.
#[must_use]
pub fn spinner(frame: u64) -> char {
    const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    FRAMES[usize::try_from(frame % FRAMES.len() as u64).unwrap_or_default()]
}

/// `s` shortened to at most `width` terminal cells by cutting out its middle and putting `…` in
/// its place. The end keeps about 40% of the room, so a file's extension stays visible:
/// `screen-recording-2026-10…hrough-final.mov`. A string that fits is returned unchanged.
#[must_use]
pub fn truncate_middle(s: &str, width: usize) -> String {
    let w = |c: char| c.width().unwrap_or(0);
    if s.chars().map(w).sum::<usize>() <= width {
        return s.to_owned();
    }
    let Some(room) = width.checked_sub(1) else {
        return String::new();
    };
    let tail_room = room * 2 / 5;
    let mut tail = Vec::new();
    let mut used = 0;
    for c in s.chars().rev() {
        if used + w(c) > tail_room {
            break;
        }
        used += w(c);
        tail.push(c);
    }
    // A combining mark cut off from its base would land on the `…`.
    while tail.last().is_some_and(|&c| w(c) == 0) {
        tail.pop();
    }
    let head_room = room - used;
    let mut head = String::with_capacity(s.len());
    used = 0;
    for c in s.chars() {
        if used + w(c) > head_room {
            break;
        }
        used += w(c);
        head.push(c);
    }
    head.push('…');
    head.extend(tail.iter().rev());
    head
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bars_have_fixed_width() {
        for f in [0.0, 0.01, 0.5, 0.999, 1.0] {
            assert_eq!(bar(f, 10, Bars::Unicode).chars().count(), 10, "frac {f}");
            assert_eq!(bar(f, 10, Bars::Ascii).chars().count(), 10, "frac {f}");
        }
        assert_eq!(bar(1.0, 4, Bars::Unicode), "████");
        assert_eq!(bar(0.5, 4, Bars::Unicode), "██  ");
        assert_eq!(bar(0.0625, 4, Bars::Unicode), "▎   ");
        assert_eq!(bar(1.0, 4, Bars::Ascii), "####");
        assert_eq!(bar(0.5, 4, Bars::Ascii), "##  ");
        assert_eq!(bar(0.0625, 4, Bars::Ascii), "    ");
        assert_eq!(bar(0.125, 4, Bars::Ascii), "#   ");
        assert!(bar(0.37, 24, Bars::Ascii).is_ascii());
    }

    #[test]
    fn formats() {
        assert_eq!(count(812), "812");
        assert_eq!(count(1_200), "1.2k");
        assert_eq!(count(3_400_000), "3.4M");
        assert_eq!(count(5_000_000_000), "5.0G");
    }

    #[test]
    fn ratios_and_spinner_frames() {
        assert!((ratio(1, 4) - 0.25).abs() < f64::EPSILON);
        assert!((percent(1, 8) - 12.5).abs() < f64::EPSILON);
        assert_eq!(spinner(0), spinner(10));
        assert_ne!(spinner(0), spinner(1));
    }

    #[test]
    fn middle_truncation_keeps_the_end() {
        let w = |s: &str| s.chars().map(|c| c.width().unwrap_or(0)).sum::<usize>();
        assert_eq!(truncate_middle("short.txt", 20), "short.txt");
        assert_eq!(truncate_middle("short.txt", 9), "short.txt");
        let long = "screen-recording-2026-10-28-walkthrough-final.mov";
        let cut = truncate_middle(long, 41);
        assert_eq!(cut, "screen-recording-2026-10…hrough-final.mov");
        for width in 0..long.len() {
            let cut = truncate_middle(long, width);
            assert!(w(&cut) <= width, "{width}: {cut}");
        }
        assert_eq!(truncate_middle(long, 1), "…");
        assert_eq!(truncate_middle(long, 0), "");
        assert!(truncate_middle(long, 20).ends_with(".mov"));
    }

    #[test]
    fn middle_truncation_counts_cells_not_chars() {
        // Each of these takes two cells.
        let wide = "日本語のファイル名がとても長い.txt";
        let cut = truncate_middle(wide, 16);
        assert!(
            cut.chars().map(|c| c.width().unwrap_or(0)).sum::<usize>() <= 16,
            "{cut}"
        );
        assert!(cut.ends_with(".txt"), "{cut}");
        assert!(cut.contains('…'), "{cut}");
        // A combining mark never starts the kept end.
        let marks = "aaaaaaaaaae\u{301}e\u{301}e\u{301}";
        for width in 1..12 {
            let cut = truncate_middle(marks, width);
            let after = cut.split('…').nth(1).unwrap_or_default();
            assert!(
                after.chars().next().is_none_or(|c| c.width() != Some(0)),
                "{width}: {cut:?}"
            );
        }
    }
}

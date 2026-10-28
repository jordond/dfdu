//! Text for numbers: compact counts, bars, the spinner.

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

/// A bar of `width` cells filled to `frac` (0..=1), with eighth-block resolution.
#[must_use]
pub fn bar(frac: f64, width: usize) -> String {
    const PARTS: [char; 8] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉'];
    let eighths = eighths(frac, width);
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
}

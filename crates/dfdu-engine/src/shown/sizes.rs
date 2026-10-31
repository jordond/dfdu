//! Which size of an entry is shown, and how sizes are written for people.

use dfdu_tree::Totals;

/// Which size the list shows.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum SizeMode {
    /// Disk usage, data shared between files counted once.
    #[default]
    Disk,
    Apparent,
    /// Disk usage as `du` counts it: shared data once per file.
    Referenced,
}

impl SizeMode {
    #[must_use]
    pub fn of(self, t: &Totals) -> u64 {
        match self {
            SizeMode::Disk => t.alloc,
            SizeMode::Apparent => t.apparent,
            SizeMode::Referenced => t.referenced,
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            SizeMode::Disk => "disk usage",
            SizeMode::Apparent => "apparent size",
            SizeMode::Referenced => "disk usage like du",
        }
    }
}

/// Human-readable size with binary units, one decimal place above bytes ("1.5 KiB", "2.0 MiB",
/// "512 B"), so the size column keeps its width.
#[must_use]
pub fn human(n: u64) -> String {
    const UNITS: [&str; 6] = ["KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    if n < 1024 {
        return format!("{n} B");
    }
    // Precision loss is fine: one decimal place is shown.
    #[expect(clippy::cast_precision_loss)]
    let mut v = n as f64 / 1024.0;
    let mut unit = 0;
    while v >= 1024.0 && unit + 1 < UNITS.len() {
        v /= 1024.0;
        unit += 1;
    }
    format!("{v:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_sizes() {
        assert_eq!(human(512), "512 B");
        assert_eq!(human(0), "0 B");
        assert_eq!(human(1536), "1.5 KiB");
        assert_eq!(human(2048), "2.0 KiB");
        assert_eq!(human(5 << 30), "5.0 GiB");
    }

    #[test]
    fn human_sizes_at_unit_boundaries() {
        assert_eq!(human(1023), "1023 B");
        assert_eq!(human(1024), "1.0 KiB");
        // Rounding up does not roll over into the next unit.
        assert_eq!(human(1_048_575), "1024.0 KiB");
        assert_eq!(human(1 << 20), "1.0 MiB");
        assert_eq!(human(1 << 40), "1.0 TiB");
        assert_eq!(human(1536 << 30), "1.5 TiB");
        assert_eq!(human(1 << 50), "1.0 PiB");
        assert_eq!(human(u64::MAX), "16.0 EiB");
    }
}

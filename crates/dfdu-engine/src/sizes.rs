//! Which size of an entry is shown, and how sizes are written for people.

use dfdu_tree::Totals;
use humansize::{BINARY, format_size};

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
    if n < 1024 {
        // humansize would print "512.0 B".
        format!("{n} B")
    } else {
        format_size(n, BINARY.decimal_places(1).decimal_zeroes(1))
    }
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
}

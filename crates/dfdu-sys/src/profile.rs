//! btrfs data profiles. The type is plain data, so a host without btrfs can receive and show
//! another host's profile; only [`crate::data_profile`] (Linux) reads one.

/// Data allocation profile of a btrfs filesystem.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Profile {
    Single,
    Dup,
    Raid0,
    Raid1,
    Raid1c3,
    Raid1c4,
    Raid10,
    Raid5,
    Raid6,
}

impl Profile {
    /// Every profile, in order: `ALL[p as usize] == p`.
    pub const ALL: [Profile; 9] = [
        Profile::Single,
        Profile::Dup,
        Profile::Raid0,
        Profile::Raid1,
        Profile::Raid1c3,
        Profile::Raid1c4,
        Profile::Raid10,
        Profile::Raid5,
        Profile::Raid6,
    ];

    /// The profile numbered `n` (`p as u8`), as sent between hosts.
    #[must_use]
    pub fn from_u8(n: u8) -> Option<Profile> {
        Profile::ALL.get(usize::from(n)).copied()
    }

    /// Raw bytes stored per byte of data, given the number of devices.
    #[must_use]
    pub fn raw_ratio(self, devices: u64) -> f64 {
        #[expect(
            clippy::cast_precision_loss,
            reason = "device counts are far below 2^52"
        )]
        let n = devices.max(1) as f64;
        match self {
            Profile::Single | Profile::Raid0 => 1.0,
            Profile::Dup | Profile::Raid1 | Profile::Raid10 => 2.0,
            Profile::Raid1c3 => 3.0,
            Profile::Raid1c4 => 4.0,
            Profile::Raid5 => n / (n - 1.0).max(1.0),
            Profile::Raid6 => n / (n - 2.0).max(1.0),
        }
    }

    /// The name `btrfs filesystem df` shows.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Profile::Single => "single",
            Profile::Dup => "DUP",
            Profile::Raid0 => "RAID0",
            Profile::Raid1 => "RAID1",
            Profile::Raid1c3 => "RAID1C3",
            Profile::Raid1c4 => "RAID1C4",
            Profile::Raid10 => "RAID10",
            Profile::Raid5 => "RAID5",
            Profile::Raid6 => "RAID6",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_round_trip() {
        for p in Profile::ALL {
            assert_eq!(Profile::from_u8(p as u8), Some(p));
        }
        assert_eq!(Profile::from_u8(9), None);
    }

    /// The numbers are part of the remote protocol: this order may not change.
    #[test]
    fn wire_numbers_are_fixed() {
        let names: Vec<_> = Profile::ALL.iter().map(|p| p.name()).collect();
        assert_eq!(
            names,
            [
                "single", "DUP", "RAID0", "RAID1", "RAID1C3", "RAID1C4", "RAID10", "RAID5", "RAID6"
            ]
        );
    }

    #[test]
    fn raw_ratio_by_profile() {
        assert!((Profile::Dup.raw_ratio(1) - 2.0).abs() < f64::EPSILON);
        assert!((Profile::Raid1c3.raw_ratio(3) - 3.0).abs() < f64::EPSILON);
        assert!((Profile::Raid5.raw_ratio(4) - 4.0 / 3.0).abs() < f64::EPSILON);
        assert!((Profile::Raid6.raw_ratio(2) - 2.0).abs() < f64::EPSILON);
    }
}

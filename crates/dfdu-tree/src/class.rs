//! What a node matched in the rules (`dfdu-rules`). The types live here rather than in
//! `dfdu-rules` because the tree stores them for every matched node.

/// Index of a rule in a scan's rule set (`dfdu_rules::RuleSet::rules`).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct RuleIx(pub u16);

/// What deleting loses. Ordered from least to most to lose,
/// so a runtime condition may only move a rule to a greater level.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[repr(u8)]
pub enum Safety {
    /// Pure cache, rebuilt transparently.
    Safe,
    /// Rebuilt on demand at a real cost (download, compile).
    Regenerable,
    /// May hold user work or state; shown with what is inside.
    Review,
    /// Reported only, or removed with the owner's own tool.
    Manual,
    /// Never offered for deletion.
    Protected,
}

impl Safety {
    /// Every level, in order: `ALL[s as usize] == s`.
    pub const ALL: [Safety; 5] = [
        Safety::Safe,
        Safety::Regenerable,
        Safety::Review,
        Safety::Manual,
        Safety::Protected,
    ];

    /// The level numbered `n` (`s as u8`), as sent between hosts.
    #[must_use]
    pub fn from_u8(n: u8) -> Option<Safety> {
        Safety::ALL.get(usize::from(n)).copied()
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Safety> {
        Some(match s {
            "safe" => Safety::Safe,
            "regenerable" => Safety::Regenerable,
            "review" => Safety::Review,
            "manual" => Safety::Manual,
            "protected" => Safety::Protected,
            _ => return None,
        })
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Safety::Safe => "safe",
            Safety::Regenerable => "regenerable",
            Safety::Review => "review",
            Safety::Manual => "manual",
            Safety::Protected => "protected",
        }
    }
}

/// The rule a node matched: its index in the scan's rule set, and the safety level and
/// `degrade` condition that hold for this node.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Class {
    pub rule: RuleIx,
    pub safety: Safety,
    /// Index into the rule's `degrade` list.
    pub degraded: Option<u16>,
    /// What the classifier pool found ([`ClassFlags::CHECKED`] and the others).
    pub flags: ClassFlags,
}

const _: () = assert!(std::mem::size_of::<Class>() == 8);

impl Class {
    /// Whether any of the flags in `f` is set.
    #[must_use]
    pub fn has(self, f: ClassFlags) -> bool {
        self.flags.has(f)
    }
}

/// What the classifier pool found for a classified node.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct ClassFlags(u8);

impl ClassFlags {
    /// No checks queued or run.
    pub const NONE: ClassFlags = ClassFlags(0);
    /// Queued for the classifier pool's checks.
    pub const QUEUED: ClassFlags = ClassFlags(1);
    /// The pool's checks ran.
    pub const CHECKED: ClassFlags = ClassFlags(2);
    /// Version control tracks files in it, so it is protected.
    pub const VCS_TRACKED: ClassFlags = ClassFlags(4);
    /// A process the rule names (`guard.not_running`) was running when checked.
    pub const IN_USE: ClassFlags = ClassFlags(8);
    /// `guard.not_vcs_tracked` asked, but git could not tell (not installed, an error, a
    /// repository it refuses): never recommended or cleaned.
    pub const VCS_UNKNOWN: ClassFlags = ClassFlags(16);

    /// The flags with these bits, as formats store them.
    #[must_use]
    pub fn from_bits(bits: u8) -> ClassFlags {
        ClassFlags(bits)
    }

    /// The flags as bits, for formats that store them.
    #[must_use]
    pub fn bits(self) -> u8 {
        self.0
    }

    /// Whether any of the flags in `f` is set.
    #[must_use]
    pub fn has(self, f: ClassFlags) -> bool {
        self.0 & f.0 != 0
    }

    pub fn set(&mut self, f: ClassFlags, on: bool) {
        if on { self.0 |= f.0 } else { self.0 &= !f.0 }
    }
}

impl std::ops::BitOr for ClassFlags {
    type Output = ClassFlags;

    fn bitor(self, rhs: ClassFlags) -> ClassFlags {
        ClassFlags(self.0 | rhs.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safety_numbers_round_trip() {
        for s in Safety::ALL {
            assert_eq!(Safety::from_u8(s as u8), Some(s));
            assert_eq!(Safety::parse(s.as_str()), Some(s));
        }
        assert_eq!(
            Safety::from_u8(u8::try_from(Safety::ALL.len()).unwrap()),
            None
        );
    }

    #[test]
    fn class_flags_set_and_clear_one_flag() {
        let mut f = ClassFlags::QUEUED | ClassFlags::IN_USE;
        f.set(ClassFlags::QUEUED, false);
        f.set(ClassFlags::CHECKED, true);
        assert!(f.has(ClassFlags::CHECKED) && f.has(ClassFlags::IN_USE));
        assert!(!f.has(ClassFlags::QUEUED));
        assert_eq!(ClassFlags::from_bits(f.bits()), f);
        assert!(!ClassFlags::default().has(ClassFlags::CHECKED | ClassFlags::QUEUED));
    }
}

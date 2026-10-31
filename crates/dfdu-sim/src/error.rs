//! Why a run failed.

use std::fmt;

/// A check of a run that did not hold, with the seed that reproduces it and, for a check made
/// as the run stepped, how far it had got.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimError {
    pub seed: u64,
    /// Steps taken when it was found; `None` for the checks at the end.
    pub ticks: Option<u64>,
    /// What did not hold.
    pub what: String,
}

impl SimError {
    /// A check at the end of the run of `seed` that did not hold.
    pub fn new(seed: u64, what: impl fmt::Display) -> SimError {
        SimError {
            seed,
            ticks: None,
            what: what.to_string(),
        }
    }

    /// The error, saying it was found in the run's `part` (of a run in parts).
    #[must_use]
    pub fn during(self, part: &str) -> SimError {
        SimError {
            what: format!("{part}: {}", self.what),
            ..self
        }
    }
}

impl fmt::Display for SimError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "seed {}: {}", self.seed, self.what)?;
        if let Some(t) = self.ticks {
            write!(f, " (after {t} ticks)")?;
        }
        Ok(())
    }
}

impl std::error::Error for SimError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_error_names_its_seed_and_how_far_the_run_got() {
        let e = SimError {
            seed: 7,
            ticks: Some(40),
            what: "budget exceeded: 9".into(),
        };
        assert_eq!(e.to_string(), "seed 7: budget exceeded: 9 (after 40 ticks)");
        let e = SimError::new(3, "classes: /sim/a: protected true").during("resume");
        assert_eq!(
            e.to_string(),
            "seed 3: resume: classes: /sim/a: protected true"
        );
    }
}

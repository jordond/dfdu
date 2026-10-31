//! What reading, building or mounting a fixture fails with: a message and what led to it, as
//! the fixture's path and entry give them (`in demo/fixtures/home.toml: entry "a": ...`).

use std::fmt;

/// A fixture that cannot be read, built or mounted. `{}` shows what failed, `{:#}` that and
/// every cause after it, and [`std::error::Error::source`] walks the causes.
pub struct Error(anyhow::Error);

impl From<anyhow::Error> for Error {
    fn from(e: anyhow::Error) -> Error {
        Error(e)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.0.chain().nth(1)
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Context as _;

    use super::Error;

    fn nested() -> Error {
        Err::<(), _>(anyhow::anyhow!("no such date"))
            .context("entry \"a\"")
            .context("in home.toml")
            .unwrap_err()
            .into()
    }

    #[test]
    fn shows_the_failure_and_its_causes() {
        let e = nested();
        assert_eq!(e.to_string(), "in home.toml");
        assert_eq!(format!("{e:#}"), "in home.toml: entry \"a\": no such date");
        let causes: Vec<_> = std::iter::successors(std::error::Error::source(&e), |c| c.source())
            .map(ToString::to_string)
            .collect();
        assert_eq!(causes, ["entry \"a\"", "no such date"]);
    }

    #[test]
    fn keeps_its_causes_inside_anyhow() {
        let wrapped = anyhow::Error::new(nested()).context("mounting");
        assert_eq!(
            format!("{wrapped:#}"),
            "mounting: in home.toml: entry \"a\": no such date"
        );
    }
}

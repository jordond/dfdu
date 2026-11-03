//! Another computer to scan as the start screen takes it, and the ones scanned lately as the
//! settings keep them.

use std::path::PathBuf;

/// The computers connected to lately, kept in the settings.
pub const RECENT: usize = 6;

/// Another computer to scan, as the start screen took it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// What ssh is given: a name from its config, `user@host`, an address.
    pub host: String,
    pub port: Option<u16>,
    /// The private key to log in with, and only it.
    pub identity: Option<PathBuf>,
    /// The folder to scan there, as typed (`~` is the home folder there).
    pub path: String,
}

impl Target {
    /// How a row of the recent ones names it: `user@nas:2222 ~/Projects`.
    pub fn label(&self) -> String {
        let port = self.port.map(|p| format!(":{p}")).unwrap_or_default();
        format!("{}{port}  {}", self.host, self.path)
    }

    /// One line of the settings: the fields between tabs. `None` for one that a tab or a line
    /// break would split.
    fn encode(&self) -> Option<String> {
        let identity = self
            .identity
            .as_ref()
            .map(|p| p.to_str().map(str::to_owned))
            .unwrap_or(Some(String::new()))?;
        let port = self.port.map(|p| p.to_string()).unwrap_or_default();
        let fields = [self.host.as_str(), &port, &identity, &self.path];
        if fields.iter().any(|f| f.contains(['\t', '\n', '\r'])) {
            return None;
        }
        Some(fields.join("\t"))
    }

    fn decode(line: &str) -> Option<Target> {
        let mut f = line.split('\t');
        let (host, port, identity, path) = (f.next()?, f.next()?, f.next()?, f.next()?);
        if host.is_empty() || f.next().is_some() {
            return None;
        }
        Some(Target {
            host: host.into(),
            port: if port.is_empty() {
                None
            } else {
                Some(port.parse().ok()?)
            },
            identity: (!identity.is_empty()).then(|| PathBuf::from(identity)),
            path: path.into(),
        })
    }
}

/// The recent targets, one a line, as the settings keep them.
pub fn encode(recent: &[Target]) -> String {
    recent
        .iter()
        .filter_map(Target::encode)
        .collect::<Vec<_>>()
        .join("\n")
}

/// The recent targets in `text`; lines that do not read are left out.
pub fn decode(text: &str) -> Vec<Target> {
    text.lines()
        .filter_map(Target::decode)
        .take(RECENT)
        .collect()
}

/// `target` first among `recent`, once, and at most [`RECENT`] of them.
pub fn remember(recent: &mut Vec<Target>, target: &Target) {
    recent.retain(|t| (&t.host, t.port, &t.path) != (&target.host, target.port, &target.path));
    recent.insert(0, target.clone());
    recent.truncate(RECENT);
}

//! The agent to send to a host: which build its platform needs, the keys it may be cached under
//! there, and where its bytes come from: `DFDU_AGENT_DIR`, this very binary, the local agent
//! cache, or the release (downloaded, with feature `agent-download`).

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

mod release;

/// The directory of agents by target (`<dir>/<target>/dfdu`), looked in first.
pub const AGENT_DIR_VAR: &str = "DFDU_AGENT_DIR";
/// Replaces the release download base, `https://github.com/jordond/dfdu/releases/download`.
pub const RELEASE_URL_VAR: &str = "DFDU_RELEASE_URL";
const RELEASE_URL: &str = "https://github.com/jordond/dfdu/releases/download";

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The release target for a host whose `uname -s` and `uname -m` are `os` and `arch`.
pub fn target_for(os: &str, arch: &str) -> Option<&'static str> {
    Some(match (os, arch) {
        ("Linux", "x86_64" | "amd64") => "x86_64-unknown-linux-musl",
        ("Linux", "aarch64" | "arm64") => "aarch64-unknown-linux-musl",
        ("Darwin", "x86_64") => "x86_64-apple-darwin",
        ("Darwin", "arm64" | "aarch64") => "aarch64-apple-darwin",
        _ => return None,
    })
}

/// What this binary is: the release target whose hosts it runs on, and its own target when
/// that differs (a glibc build, which may not run on a host with an older glibc).
#[derive(Clone, Copy, Debug)]
pub struct Own {
    pub target: &'static str,
    pub glibc: Option<&'static str>,
}

fn own() -> Option<Own> {
    let os = match std::env::consts::OS {
        "linux" => "Linux",
        "macos" => "Darwin",
        _ => return None,
    };
    let target = target_for(os, std::env::consts::ARCH)?;
    let glibc = cfg!(all(target_os = "linux", not(target_env = "musl"))).then(|| {
        if std::env::consts::ARCH == "x86_64" {
            "x86_64-unknown-linux-gnu"
        } else {
            "aarch64-unknown-linux-gnu"
        }
    });
    Some(Own { target, glibc })
}

/// `bytes`' SHA-256, in lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// One agent that could be sent.
#[derive(Debug, PartialEq, Eq)]
pub struct Candidate {
    /// The key it is cached under on the host.
    pub key: String,
    /// The target it was built for.
    pub built_for: &'static str,
    pub from: Origin,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Origin {
    /// A file here, keyed by its hash (`prefix-hash`).
    File { path: PathBuf, prefix: &'static str },
    /// The release, in the local agent cache.
    Cached(PathBuf),
    /// The release, to download.
    Release,
}

impl Candidate {
    /// Its size, when known without downloading it.
    pub fn size(&self) -> Option<u64> {
        match &self.from {
            Origin::File { path, .. } | Origin::Cached(path) => Some(path.metadata().ok()?.len()),
            Origin::Release => None,
        }
    }

    /// Whether it is this very binary.
    pub fn is_this(&self) -> bool {
        matches!(self.from, Origin::File { prefix: "self", .. })
    }
}

/// Finds the agents to send.
pub struct Agents {
    /// `DFDU_AGENT_DIR`.
    pub agent_dir: Option<PathBuf>,
    /// This binary, and what it is.
    pub this: Option<(PathBuf, Own)>,
    /// The local agent cache (`<dir>/<version>-<target>/dfdu`).
    pub cache: Option<PathBuf>,
    /// The release download base.
    pub release_url: String,
}

impl Agents {
    /// From the environment.
    pub fn from_env() -> Agents {
        Agents {
            agent_dir: std::env::var_os(AGENT_DIR_VAR).map(PathBuf::from),
            this: std::env::current_exe().ok().zip(own()),
            cache: dfdu_sys::dirs::agent_cache_dir(),
            release_url: std::env::var(RELEASE_URL_VAR).unwrap_or_else(|_| RELEASE_URL.into()),
        }
    }

    /// The agents for `target`, preferred first: the one in `DFDU_AGENT_DIR`; else this
    /// binary when it is a release target's; else the release, then this binary when it only
    /// matches the platform (a glibc build, sent when there is no release to download).
    pub fn plan(&self, target: &'static str) -> io::Result<Vec<Candidate>> {
        let file = |path: PathBuf, prefix, built_for| -> io::Result<Candidate> {
            let hash = sha256_hex(&read(&path)?);
            Ok(Candidate {
                key: format!("{prefix}-{}", &hash[..16]),
                built_for,
                from: Origin::File { path, prefix },
            })
        };
        if let Some(d) = &self.agent_dir {
            let path = d.join(target).join("dfdu");
            if path.is_file() {
                return Ok(vec![file(path, "dir", target)?]);
            }
        }
        let this = self.this.as_ref().filter(|(_, o)| o.target == target);
        if let Some((path, Own { glibc: None, .. })) = this {
            return Ok(vec![file(path.clone(), "self", target)?]);
        }
        let release = match self.cached(target).filter(|p| p.is_file()) {
            Some(path) => Origin::Cached(path),
            None => Origin::Release,
        };
        let mut plan = vec![Candidate {
            key: release_key(target),
            built_for: target,
            from: release,
        }];
        if let Some((path, Own { glibc: Some(g), .. })) = this {
            plan.push(file(path.clone(), "self", g)?);
        }
        Ok(plan)
    }

    fn cached(&self, target: &str) -> Option<PathBuf> {
        Some(self.cache.as_ref()?.join(release_key(target)).join("dfdu"))
    }

    /// `c`'s bytes and the key they go under; a release is downloaded and kept in the local
    /// cache.
    pub fn load(&self, target: &str, c: &Candidate) -> io::Result<(String, Vec<u8>)> {
        match &c.from {
            Origin::File { path, prefix } => {
                // Keyed by what is sent, should the file have changed since the plan.
                let bin = read(path)?;
                Ok((format!("{prefix}-{}", &sha256_hex(&bin)[..16]), bin))
            }
            Origin::Cached(path) => Ok((c.key.clone(), read(path)?)),
            Origin::Release => {
                let bin = release::download(&self.release_url, target)?;
                if let Some(path) = self.cached(target) {
                    // A cache that cannot be written costs a download next time, nothing more.
                    let _ = store(&path, &bin);
                }
                Ok((c.key.clone(), bin))
            }
        }
    }
}

/// The key of a release agent, also its directory in the local cache.
fn release_key(target: &str) -> String {
    format!("{VERSION}-{target}")
}

fn read(path: &Path) -> io::Result<Vec<u8>> {
    std::fs::read(path).map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))
}

/// Writes `bin` to `path` whole or not at all, durably: a cached agent is sent without a
/// checksum, so one cut short by a crash must never be left under its name.
fn store(path: &Path, bin: &[u8]) -> io::Result<()> {
    let dir = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
    std::fs::create_dir_all(dir)?;
    dfdu_store::write_atomic(path, |f| f.write_all(bin))
}

#[cfg(test)]
mod tests;

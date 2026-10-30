//! The agent to send to a host: which build its platform needs, the keys it may be cached under
//! there, and where its bytes come from: `DFDU_AGENT_DIR`, this very binary, the local agent
//! cache, or the release (downloaded, with feature `agent-download`).

use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// The directory of agents by target (`<dir>/<target>/dfdu`), looked in first.
pub const AGENT_DIR_VAR: &str = "DFDU_AGENT_DIR";
/// Replaces the release download base, `https://github.com/jordond/dfdu/releases/download`.
pub const RELEASE_URL_VAR: &str = "DFDU_RELEASE_URL";
const RELEASE_URL: &str = "https://github.com/jordond/dfdu/releases/download";
/// Releases are below these sizes; larger downloads, or binaries in them, are refused.
#[cfg(feature = "agent-download")]
const MAX_ARCHIVE: u64 = 128 << 20;
#[cfg(feature = "agent-download")]
const MAX_BINARY: u64 = 256 << 20;

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
    #[cfg_attr(
        not(feature = "agent-download"),
        expect(dead_code, reason = "read only by the download")
    )]
    pub release_url: String,
}

impl Agents {
    /// From the environment.
    pub fn from_env() -> Agents {
        Agents {
            agent_dir: std::env::var_os(AGENT_DIR_VAR).map(PathBuf::from),
            this: std::env::current_exe().ok().zip(own()),
            cache: dfdu_rules::dirs::agent_cache_dir(),
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
                let bin = self.download(target)?;
                if let Some(path) = self.cached(target) {
                    // A cache that cannot be written costs a download next time, nothing more.
                    let _ = store(&path, &bin);
                }
                Ok((c.key.clone(), bin))
            }
        }
    }

    #[cfg(feature = "agent-download")]
    fn download(&self, target: &str) -> io::Result<Vec<u8>> {
        let name = format!("dfdu-{target}.tar.xz");
        let url = format!(
            "{}/v{VERSION}/{name}",
            self.release_url.trim_end_matches('/')
        );
        let missing = || {
            io::Error::other(format!(
                "no dfdu {VERSION} release for {target} ({url}); install dfdu there, or set \
                 {AGENT_DIR_VAR} to a directory of <target>/dfdu builds"
            ))
        };
        let archive = get(&url, MAX_ARCHIVE).map_err(|e| match e {
            ureq::Error::StatusCode(404) => missing(),
            e => io::Error::other(format!("downloading {url}: {e}")),
        })?;
        let sums = get(&format!("{url}.sha256"), 4096)
            .map_err(|e| io::Error::other(format!("downloading {url}.sha256: {e}")))?;
        let want = String::from_utf8_lossy(&sums);
        let want = want.split_whitespace().next().unwrap_or_default();
        if !want.eq_ignore_ascii_case(&sha256_hex(&archive)) {
            return Err(io::Error::other(format!(
                "{name} does not match its published sha256"
            )));
        }
        extract(&archive, target)
    }

    #[cfg(not(feature = "agent-download"))]
    fn download(&self, target: &str) -> io::Result<Vec<u8>> {
        Err(io::Error::other(format!(
            "this dfdu was built without downloads, and has no agent for {target}; install \
             dfdu there, or set {AGENT_DIR_VAR} to a directory of <target>/dfdu builds"
        )))
    }
}

/// The key of a release agent, also its directory in the local cache.
fn release_key(target: &str) -> String {
    format!("{VERSION}-{target}")
}

fn read(path: &Path) -> io::Result<Vec<u8>> {
    std::fs::read(path).map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))
}

/// Writes `bin` to `path` whole or not at all.
fn store(path: &Path, bin: &[u8]) -> io::Result<()> {
    let dir = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".dfdu.{}", std::process::id()));
    std::fs::write(&tmp, bin)
        .and_then(|()| std::fs::rename(&tmp, path))
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
}

/// `url`'s body, up to `limit` bytes, through the system's trusted certificates and the
/// proxy in the environment. Gives up on a connection that stalls.
#[cfg(feature = "agent-download")]
fn get(url: &str, limit: u64) -> Result<Vec<u8>, ureq::Error> {
    use std::time::Duration;
    use ureq::tls::{RootCerts, TlsConfig};
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .tls_config(
            TlsConfig::builder()
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        )
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_per_read(Some(Duration::from_secs(60)))
        .build()
        .into();
    agent
        .get(url)
        .call()?
        .body_mut()
        .with_config()
        .limit(limit)
        .read_to_vec()
}

/// The `dfdu` binary in a release archive: a `.tar.xz` whose top directory is
/// `dfdu-<target>`.
#[cfg(feature = "agent-download")]
pub fn extract(archive: &[u8], target: &str) -> io::Result<Vec<u8>> {
    use std::io::Read;
    let want = Path::new(&format!("dfdu-{target}")).join("dfdu");
    let xz = lzma_rust2::XzReader::new(archive, false);
    let mut tar = tar::Archive::new(xz);
    for entry in tar.entries()? {
        let entry = entry?;
        if entry.header().entry_type().is_file() && *entry.path()? == *want {
            if entry.size() > MAX_BINARY {
                return Err(io::Error::other(format!(
                    "{} in the release archive is too large",
                    want.display()
                )));
            }
            let mut bin = Vec::new();
            entry.take(MAX_BINARY).read_to_end(&mut bin)?;
            return Ok(bin);
        }
    }
    Err(io::Error::other(format!(
        "the release archive has no {}",
        want.display()
    )))
}

#[cfg(test)]
mod tests;

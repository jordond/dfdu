//! Downloading a release agent: the archive for a target from the release download base,
//! checked against its published SHA-256, and the `dfdu` binary in it.

use std::io;

use super::AGENT_DIR_VAR;
#[cfg(feature = "agent-download")]
use super::{VERSION, sha256_hex};

/// Releases are below these sizes; larger downloads, or binaries in them, are refused.
#[cfg(feature = "agent-download")]
const MAX_ARCHIVE: u64 = 128 << 20;
#[cfg(feature = "agent-download")]
const MAX_BINARY: u64 = 256 << 20;

/// The release agent for `target`, downloaded from below `base` and checked against its
/// published sha256.
#[cfg(feature = "agent-download")]
pub fn download(base: &str, target: &str) -> io::Result<Vec<u8>> {
    let name = format!("dfdu-{target}.tar.xz");
    let url = format!("{}/v{VERSION}/{name}", base.trim_end_matches('/'));
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

/// Without downloads there is no release agent: says what to do instead.
#[cfg(not(feature = "agent-download"))]
pub fn download(_base: &str, target: &str) -> io::Result<Vec<u8>> {
    Err(io::Error::other(format!(
        "this dfdu was built without downloads, and has no agent for {target}; install dfdu \
         there, or set {AGENT_DIR_VAR} to a directory of <target>/dfdu builds"
    )))
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
    use std::path::Path;
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
pub(super) mod tests;

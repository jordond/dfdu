//! Downloading a release agent: the archive for a target from the release download base,
//! checked against its published SHA-256, and the `dfdu` binary in it.

use std::io;

use super::AGENT_DIR_VAR;
#[cfg(feature = "agent-download")]
use super::{VERSION, sha256_hex};
#[cfg(feature = "agent-download")]
use dfdu_update::net::get;

/// Releases are below these sizes; larger downloads are refused.
#[cfg(feature = "agent-download")]
const MAX_ARCHIVE: u64 = dfdu_update::cli::MAX_ARCHIVE;

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
    let archive = get(&url, MAX_ARCHIVE).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => missing(),
        _ => io::Error::other(format!("downloading {e}")),
    })?;
    let sums = get(&format!("{url}.sha256"), 4096)
        .map_err(|e| io::Error::other(format!("downloading {e}")))?;
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

/// The `dfdu` binary in a release archive: a `.tar.xz` whose top directory is
/// `dfdu-<target>`.
#[cfg(feature = "agent-download")]
pub fn extract(archive: &[u8], target: &str) -> io::Result<Vec<u8>> {
    dfdu_update::cli::unpack(archive, target).map(|files| files.bin)
}

#[cfg(test)]
pub(super) mod tests;

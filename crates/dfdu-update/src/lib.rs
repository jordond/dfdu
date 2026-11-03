//! Updates for the CLI (`dfdu update`) and the GUI: which release is the latest
//! ([`Releases::latest`]), its assets downloaded and checked against their published SHA-256
//! ([`Releases::fetch`]), the CLI's archive unpacked and its binary put in place over a running
//! copy ([`cli`]), and what manages a copy that should be updated through it instead
//! ([`cli::Managed`]).
//!
//! The release server is GitHub's (`DFDU_RELEASE_BASE` replaces it, as for install.sh). With
//! feature `net` off nothing is downloaded and every download says so.

pub mod cli;
mod release;
mod version;

use sha2::{Digest, Sha256};

pub use release::{BASE_VAR, RELEASES, Releases, net};
pub use version::Version;

/// This build's version: the CLI and the GUI share the workspace's.
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// Turns off the GUI's automatic update check when set to anything but `0` or empty.
pub const NO_CHECK_VAR: &str = "DFDU_NO_UPDATE_CHECK";

/// Whether `DFDU_NO_UPDATE_CHECK` turns automatic checks off.
pub fn checks_disabled() -> bool {
    std::env::var_os(NO_CHECK_VAR).is_some_and(|v| !v.is_empty() && v != "0")
}

/// `bytes`' SHA-256, in lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[cfg(all(test, feature = "net"))]
mod tests;

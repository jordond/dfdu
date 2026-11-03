//! The release server: which release is the latest, and a release's assets, each checked
//! against the `.sha256` published beside it.
//!
//! The latest release is read from where `<base>/latest` redirects to (`<base>/tag/vX.Y.Z`),
//! so no API (and none of its rate limit) is involved. Prereleases are never the latest.
//! Everything goes over https, redirects included, unless the base is overridden with an http
//! one (tests).

use std::io;

use crate::Version;

/// GitHub's releases of dfdu.
pub const RELEASES: &str = "https://github.com/jordond/dfdu/releases";
/// Replaces [`RELEASES`], as it does for install.sh; plain http is allowed (for tests).
pub const BASE_VAR: &str = "DFDU_RELEASE_BASE";

/// Where releases come from.
#[derive(Clone, Debug)]
pub struct Releases {
    base: String,
}

impl Releases {
    /// Below `base`, laid out as GitHub's releases are.
    pub fn new(base: &str) -> Self {
        Releases {
            base: base.trim_end_matches('/').to_owned(),
        }
    }

    /// [`RELEASES`], or `DFDU_RELEASE_BASE` when set.
    pub fn from_env() -> Self {
        match std::env::var(BASE_VAR) {
            Ok(base) if !base.is_empty() => Releases::new(&base),
            _ => Releases::new(RELEASES),
        }
    }

    /// The page of release `v`, with its notes.
    pub fn page(&self, v: &Version) -> String {
        format!("{}/tag/v{v}", self.base)
    }

    /// Where asset `name` of release `v` is downloaded from.
    pub fn asset_url(&self, v: &Version, name: &str) -> String {
        format!("{}/download/v{v}/{name}", self.base)
    }

    /// The latest release's version.
    ///
    /// # Errors
    /// The server cannot be reached, has no release yet, or answers with something else than a
    /// redirect to a release's tag.
    pub fn latest(&self) -> io::Result<Version> {
        let url = format!("{}/latest", self.base);
        let location = net::redirect(&url)?;
        // `.../releases/tag/v0.3.0`; a repository without a release redirects to `.../releases`.
        let tag = location
            .split(['?', '#'])
            .next()
            .and_then(|l| l.trim_end_matches('/').rsplit_once("/tag/"))
            .map(|(_, tag)| tag);
        tag.and_then(Version::parse).ok_or_else(|| {
            io::Error::other(format!(
                "{url} does not lead to a release (it leads to {location})"
            ))
        })
    }

    /// Asset `name` of release `v`, at most `limit` bytes, checked against `name.sha256`.
    ///
    /// # Errors
    /// [`io::ErrorKind::NotFound`] when the release has no such asset; otherwise the download
    /// failed, the checksum is missing or does not match.
    pub fn fetch(&self, v: &Version, name: &str, limit: u64) -> io::Result<Vec<u8>> {
        let url = self.asset_url(v, name);
        let body = net::get(&url, limit)?;
        let sums = net::get(&format!("{url}.sha256"), 4096).map_err(|e| {
            io::Error::new(e.kind(), format!("{name} has no published checksum: {e}"))
        })?;
        let sums = String::from_utf8_lossy(&sums);
        let want = sums.split_whitespace().next().unwrap_or_default();
        if want.len() != 64 || !want.eq_ignore_ascii_case(&crate::sha256_hex(&body)) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{name} does not match its published SHA-256"),
            ));
        }
        Ok(body)
    }
}

#[cfg(feature = "net")]
pub mod net {
    //! HTTP through ureq: the system's trusted certificates and the proxy in the environment.

    use std::io;
    use std::time::Duration;

    use ureq::tls::{RootCerts, TlsConfig};

    /// The agent for `url`: https only (redirects too) unless `url` is plain http, which only
    /// an overridden base is; no proxy for this machine's own address (a test's server).
    fn agent(url: &str, redirects: u32) -> ureq::Agent {
        let mut config = ureq::Agent::config_builder()
            .tls_config(
                TlsConfig::builder()
                    .root_certs(RootCerts::PlatformVerifier)
                    .build(),
            )
            .https_only(url.starts_with("https://"))
            .max_redirects(redirects);
        let authority = url
            .split_once("://")
            .map_or("", |(_, rest)| rest)
            .split('/')
            .next()
            .unwrap_or_default();
        let host = match authority.split_once(']') {
            Some((v6, _)) => v6.trim_start_matches('['),
            None => authority.split(':').next().unwrap_or_default(),
        };
        if ["127.0.0.1", "localhost", "::1"].contains(&host) {
            config = config.proxy(None);
        }
        config
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_per_read(Some(Duration::from_secs(60)))
            .build()
            .into()
    }

    fn error(url: &str, e: ureq::Error) -> io::Error {
        match e {
            ureq::Error::StatusCode(404) => {
                io::Error::new(io::ErrorKind::NotFound, format!("{url}: not found"))
            }
            ureq::Error::Io(e) => io::Error::new(e.kind(), format!("{url}: {e}")),
            e => io::Error::other(format!("{url}: {e}")),
        }
    }

    /// `url`'s body, up to `limit` bytes. Gives up on a connection that stalls.
    ///
    /// # Errors
    /// [`io::ErrorKind::NotFound`] for a 404; any other failure with the URL in its message.
    pub fn get(url: &str, limit: u64) -> io::Result<Vec<u8>> {
        agent(url, 10)
            .get(url)
            .call()
            .and_then(|mut r| r.body_mut().with_config().limit(limit).read_to_vec())
            .map_err(|e| error(url, e))
    }

    /// Where `url` redirects to.
    pub(crate) fn redirect(url: &str) -> io::Result<String> {
        let r = agent(url, 0).get(url).call().map_err(|e| error(url, e))?;
        if !r.status().is_redirection() {
            return Err(io::Error::other(format!(
                "{url}: expected a redirect, got {}",
                r.status()
            )));
        }
        r.headers()
            .get("location")
            .and_then(|l| l.to_str().ok())
            .map(str::to_owned)
            .ok_or_else(|| io::Error::other(format!("{url}: a redirect without a location")))
    }
}

#[cfg(not(feature = "net"))]
pub mod net {
    //! Without the `net` feature: every download says so.

    use std::io;

    fn none() -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "this dfdu was built without downloads",
        )
    }

    /// # Errors
    /// Always: there is no download.
    pub fn get(_url: &str, _limit: u64) -> io::Result<Vec<u8>> {
        Err(none())
    }

    pub(crate) fn redirect(_url: &str) -> io::Result<String> {
        Err(none())
    }
}

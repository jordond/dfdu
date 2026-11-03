#![allow(clippy::unwrap_used, clippy::expect_used)]

#[cfg(feature = "agent-download")]
mod download;

#[cfg(feature = "agent-download")]
pub use download::{published, serve};

#[cfg(not(feature = "agent-download"))]
#[test]
fn without_downloads_there_is_no_release_agent() {
    use super::*;
    let target = "aarch64-unknown-linux-musl";
    let err = download("http://127.0.0.1:1", target)
        .unwrap_err()
        .to_string();
    assert!(err.contains("built without downloads"), "{err}");
    assert!(err.contains(target), "{err}");
    assert!(err.contains(AGENT_DIR_VAR), "{err}");
}

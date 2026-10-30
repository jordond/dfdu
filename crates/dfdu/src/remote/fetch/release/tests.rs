#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

const MUSL: &str = "x86_64-unknown-linux-musl";
const ARM: &str = "aarch64-unknown-linux-musl";

/// A release archive holding `files` (path, bytes).
pub fn tar_xz(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut tar = tar::Builder::new(Vec::new());
    for (name, data) in files {
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(0o755);
        h.set_cksum();
        tar.append_data(&mut h, name, *data).unwrap();
    }
    let tar = tar.into_inner().unwrap();
    let mut xz =
        lzma_rust2::XzWriter::new(Vec::new(), lzma_rust2::XzOptions::with_preset(1)).unwrap();
    io::Write::write_all(&mut xz, &tar).unwrap();
    xz.finish().unwrap()
}

/// Serves `files` (path, body) over HTTP, one request per connection; the base URL.
pub fn serve(files: Vec<(String, Vec<u8>)>) -> String {
    use std::io::{BufRead, BufReader, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { break };
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut first = String::new();
            r.read_line(&mut first).unwrap();
            loop {
                let mut h = String::new();
                if r.read_line(&mut h).unwrap() == 0 || h == "\r\n" {
                    break;
                }
            }
            let path = first.split(' ').nth(1).unwrap_or_default();
            let (status, body) = match files.iter().find(|(p, _)| p == path) {
                Some((_, body)) => ("200 OK", body.as_slice()),
                None => ("404 Not Found", &[][..]),
            };
            write!(
                s,
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            s.write_all(body).unwrap();
        }
    });
    format!("http://{addr}")
}

/// What the release download base serves for `target`: its archive holding `agent`, and the
/// archive's sha256 (`sum` instead when given).
pub fn published(target: &str, agent: &[u8], sum: Option<&str>) -> Vec<(String, Vec<u8>)> {
    let archive = tar_xz(&[(&format!("dfdu-{target}/dfdu"), agent)]);
    let sum = sum.map_or_else(|| sha256_hex(&archive), str::to_string);
    let path = format!("/v{VERSION}/dfdu-{target}.tar.xz");
    vec![
        (
            format!("{path}.sha256"),
            format!("{sum}  dfdu-{target}.tar.xz\n").into_bytes(),
        ),
        (path, archive),
    ]
}

#[test]
fn extracts_dfdu_from_its_top_directory() {
    let archive = tar_xz(&[
        ("dfdu-x86_64-unknown-linux-musl/README.md", b"read me"),
        ("dfdu-x86_64-unknown-linux-musl/dfdu", b"\x7fELF agent"),
    ]);
    assert_eq!(extract(&archive, MUSL).unwrap(), b"\x7fELF agent");
    let without = tar_xz(&[("dfdu-x86_64-unknown-linux-musl/other", b"x")]);
    assert!(
        extract(&without, MUSL)
            .unwrap_err()
            .to_string()
            .contains("has no")
    );
    assert!(extract(b"not xz", MUSL).is_err());
}

#[test]
fn downloads_the_binary_from_a_checked_archive() {
    let base = serve(published(ARM, b"agent bytes", None));
    assert_eq!(download(&base, ARM).unwrap(), b"agent bytes");
    // A trailing slash on the base is fine.
    assert_eq!(download(&format!("{base}/"), ARM).unwrap(), b"agent bytes");
}

#[test]
fn an_archive_that_does_not_match_its_sum_is_refused() {
    let base = serve(published(ARM, b"agent bytes", Some(&"0".repeat(64))));
    let err = download(&base, ARM).unwrap_err().to_string();
    assert!(err.contains("does not match its published sha256"), "{err}");
}

#[test]
fn a_missing_release_says_what_to_do_instead() {
    let base = serve(Vec::new());
    let err = download(&base, ARM).unwrap_err().to_string();
    assert!(
        err.contains(&format!("no dfdu {VERSION} release for {ARM}")),
        "{err}"
    );
    assert!(err.contains(AGENT_DIR_VAR), "{err}");
}

#[test]
fn a_release_with_no_sum_is_refused() {
    let mut files = published(ARM, b"agent bytes", None);
    files.retain(|(p, _)| !p.ends_with(".sha256"));
    let err = download(&serve(files), ARM).unwrap_err().to_string();
    assert!(err.contains(".sha256"), "{err}");
}

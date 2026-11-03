#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io;

use super::*;

/// What the fake server answers for a path.
pub enum Answer {
    Body(Vec<u8>),
    Redirect(String),
}

/// Serves `files` over HTTP, one request per connection; the base URL.
pub fn serve(files: Vec<(String, Answer)>) -> String {
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
            let (head, body) = match files.iter().find(|(p, _)| p == path) {
                Some((_, Answer::Body(b))) => ("200 OK".to_owned(), b.as_slice()),
                Some((_, Answer::Redirect(to))) => {
                    (format!("302 Found\r\nLocation: {to}"), &[][..])
                }
                None => ("404 Not Found".to_owned(), &[][..]),
            };
            let _ = write!(
                s,
                "HTTP/1.1 {head}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = s.write_all(body);
        }
    });
    format!("http://{addr}")
}

/// A `.tar.xz` holding `files` (path, bytes).
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

/// Asset `name` of release `v` holding `body`, with its checksum (`sum` instead when given).
pub fn asset(v: &str, name: &str, body: Vec<u8>, sum: Option<&str>) -> Vec<(String, Answer)> {
    let sum = sum.map_or_else(|| sha256_hex(&body), str::to_owned);
    let path = format!("/download/v{v}/{name}");
    vec![
        (
            format!("{path}.sha256"),
            Answer::Body(format!("{sum}  {name}\n").into_bytes()),
        ),
        (path, Answer::Body(body)),
    ]
}

#[test]
fn the_latest_release_is_where_latest_redirects() {
    let base = serve(vec![(
        "/latest".into(),
        Answer::Redirect("https://github.com/jordond/dfdu/releases/tag/v0.4.1".into()),
    )]);
    assert_eq!(
        Releases::new(&base).latest().unwrap(),
        Version::parse("0.4.1").unwrap()
    );
    // A trailing slash on the base is fine.
    assert_eq!(
        Releases::new(&format!("{base}/"))
            .latest()
            .unwrap()
            .to_string(),
        "0.4.1"
    );
}

#[test]
fn no_release_or_no_redirect_is_an_error() {
    let base = serve(vec![(
        "/latest".into(),
        Answer::Redirect("https://github.com/jordond/dfdu/releases".into()),
    )]);
    let e = Releases::new(&base).latest().unwrap_err().to_string();
    assert!(e.contains("does not lead to a release"), "{e}");

    let base = serve(vec![("/latest".into(), Answer::Body(b"<html>".to_vec()))]);
    let e = Releases::new(&base).latest().unwrap_err().to_string();
    assert!(e.contains("expected a redirect"), "{e}");

    let e = Releases::new(&serve(Vec::new())).latest().unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::NotFound);
}

#[test]
fn assets_are_checked_against_their_sums() {
    let v = Version::parse("0.4.0").unwrap();
    let base = serve(asset("0.4.0", "a.bin", b"bytes".to_vec(), None));
    let r = Releases::new(&base);
    assert_eq!(r.fetch(&v, "a.bin", 100).unwrap(), b"bytes");
    // Too large for the limit.
    assert!(r.fetch(&v, "a.bin", 2).is_err());
    assert_eq!(
        r.fetch(&v, "b.bin", 100).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );

    let bad = serve(asset(
        "0.4.0",
        "a.bin",
        b"bytes".to_vec(),
        Some(&"0".repeat(64)),
    ));
    let e = Releases::new(&bad).fetch(&v, "a.bin", 100).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::InvalidData);
    assert!(e.to_string().contains("does not match"), "{e}");

    let mut unsummed = asset("0.4.0", "a.bin", b"bytes".to_vec(), None);
    unsummed.retain(|(p, _)| !p.ends_with(".sha256"));
    let e = Releases::new(&serve(unsummed))
        .fetch(&v, "a.bin", 100)
        .unwrap_err();
    assert!(e.to_string().contains("no published checksum"), "{e}");
}

#[test]
fn a_cli_archive_unpacks_to_its_binary_and_extras() {
    let t = "x86_64-unknown-linux-musl";
    let archive = tar_xz(&[
        ("dfdu-x86_64-unknown-linux-musl/README.md", b"read me"),
        ("dfdu-x86_64-unknown-linux-musl/dfdu", b"\x7fELF"),
        ("dfdu-x86_64-unknown-linux-musl/man/dfdu.1", b"man"),
        (
            "dfdu-x86_64-unknown-linux-musl/man/dfdu-scan.1",
            b"man scan",
        ),
        ("dfdu-x86_64-unknown-linux-musl/completions/_dfdu", b"zsh"),
        ("dfdu-x86_64-unknown-linux-musl/completions/dfdu.ps1", b"ps"),
        ("other/dfdu", b"not this one"),
    ]);
    let files = cli::unpack(&archive, t).unwrap();
    assert_eq!(files.bin, b"\x7fELF");
    let mut names: Vec<_> = files.extras.iter().map(|(n, _)| n.as_str()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        ["completions/_dfdu", "man/dfdu-scan.1", "man/dfdu.1"]
    );

    let without = tar_xz(&[("dfdu-x86_64-unknown-linux-musl/other", b"x")]);
    let e = cli::unpack(&without, t).unwrap_err().to_string();
    assert!(e.contains("has no"), "{e}");
    assert!(cli::unpack(b"not xz", t).is_err());
}

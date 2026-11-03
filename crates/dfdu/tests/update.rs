//! `dfdu update` against a fake release server: a copy of dfdu replaces itself with the
//! release's binary and refreshes the extras already installed; `--check` changes nothing.
#![cfg(all(unix, feature = "self-update"))]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::Command;

/// Serves `/latest` as a redirect to `tag`, and `files` (path, body); the base URL.
fn serve(tag: &str, files: Vec<(String, Vec<u8>)>) -> String {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let tag = tag.to_owned();
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
            let (head, body) = if path == "/latest" {
                (format!("302 Found\r\nLocation: /tag/{tag}"), &[][..])
            } else {
                match files.iter().find(|(p, _)| p == path) {
                    Some((_, b)) => ("200 OK".to_owned(), b.as_slice()),
                    None => ("404 Not Found".to_owned(), &[][..]),
                }
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

fn tar_xz(files: &[(String, &[u8])]) -> Vec<u8> {
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
    xz.write_all(&tar).unwrap();
    xz.finish().unwrap()
}

/// A release `version` of this machine's archive, holding `bin` and a man page.
fn release(version: &str, bin: &[u8]) -> Vec<(String, Vec<u8>)> {
    let target = dfdu_update::cli::target().unwrap();
    let name = dfdu_update::cli::asset(target);
    let archive = tar_xz(&[
        (format!("dfdu-{target}/dfdu"), bin),
        (format!("dfdu-{target}/man/dfdu.1"), b"new man page"),
        (format!("dfdu-{target}/completions/_dfdu"), b"new zsh"),
    ]);
    let sum = format!("{}  {name}\n", dfdu_update::sha256_hex(&archive));
    let path = format!("/download/v{version}/{name}");
    vec![
        (format!("{path}.sha256"), sum.into_bytes()),
        (path, archive),
    ]
}

/// A copy of dfdu in `dir/bin`, run with `args` against `base`, extras in `dir/share`.
fn run(dir: &Path, base: &str, args: &[&str]) -> (bool, String) {
    let exe = dir.join("bin").join("dfdu");
    if !exe.exists() {
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::copy(env!("CARGO_BIN_EXE_dfdu"), &exe).unwrap();
    }
    let out = Command::new(&exe)
        .arg("update")
        .args(args)
        .env("DFDU_RELEASE_BASE", base)
        .env("XDG_DATA_HOME", dir.join("share"))
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    (out.status.success(), text)
}

#[test]
fn a_newer_release_replaces_this_binary_and_its_installed_extras() {
    let dir = tempfile::tempdir().unwrap();
    let base = serve("v99.0.0", release("99.0.0", b"#!/bin/sh\necho new\n"));
    let man = dir.path().join("share/man/man1/dfdu.1");
    std::fs::create_dir_all(man.parent().unwrap()).unwrap();
    std::fs::write(&man, "old man page").unwrap();

    let (ok, text) = run(dir.path(), &base, &["--check"]);
    assert!(ok, "{text}");
    assert!(text.contains("dfdu 99.0.0 is out"), "{text}");
    assert!(text.contains(&format!("{base}/tag/v99.0.0")), "{text}");
    let exe = dir.path().join("bin/dfdu");
    assert_ne!(std::fs::read(&exe).unwrap(), b"#!/bin/sh\necho new\n");

    let (ok, text) = run(dir.path(), &base, &[]);
    assert!(ok, "{text}");
    assert!(text.contains("to dfdu 99.0.0"), "{text}");
    assert_eq!(std::fs::read(&exe).unwrap(), b"#!/bin/sh\necho new\n");
    // The man page that was there is refreshed; zsh's completion, never installed, is not.
    assert_eq!(std::fs::read(&man).unwrap(), b"new man page");
    assert!(!dir.path().join("share/zsh").exists());
}

#[test]
fn an_older_or_equal_release_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let base = serve("v0.0.1", release("0.0.1", b"old"));
    let (ok, text) = run(dir.path(), &base, &[]);
    assert!(ok, "{text}");
    assert!(text.contains("is the latest release"), "{text}");
    assert_ne!(std::fs::read(dir.path().join("bin/dfdu")).unwrap(), b"old");
}

#[test]
fn a_download_that_does_not_match_its_sum_is_not_installed() {
    let dir = tempfile::tempdir().unwrap();
    let mut files = release("99.0.0", b"new");
    files[0].1 = format!("{}  x\n", "0".repeat(64)).into_bytes();
    let base = serve("v99.0.0", files);
    let (ok, text) = run(dir.path(), &base, &[]);
    assert!(!ok, "{text}");
    assert!(
        text.contains("does not match its published SHA-256"),
        "{text}"
    );
    assert_ne!(std::fs::read(dir.path().join("bin/dfdu")).unwrap(), b"new");
}

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

const MUSL: &str = "x86_64-unknown-linux-musl";
const ARM: &str = "aarch64-unknown-linux-musl";
/// Nothing listens there: a download fails at once.
const NOWHERE: &str = "http://127.0.0.1:1";

#[test]
fn platforms_map_to_release_targets() {
    for (os, arch, target) in [
        ("Linux", "x86_64", MUSL),
        ("Linux", "aarch64", ARM),
        ("Linux", "arm64", ARM),
        ("Darwin", "arm64", "aarch64-apple-darwin"),
        ("Darwin", "x86_64", "x86_64-apple-darwin"),
    ] {
        assert_eq!(target_for(os, arch), Some(target), "{os} {arch}");
    }
    assert_eq!(target_for("Linux", "armv7l"), None);
    assert_eq!(target_for("FreeBSD", "amd64"), None);
    assert_eq!(target_for("MINGW64_NT-10.0", "x86_64"), None);
}

/// Agents in `tmp`, this binary being `tmp/this` built for `own` (`glibc` if a glibc build).
fn agents(tmp: &Path, own: &'static str, glibc: Option<&'static str>) -> Agents {
    std::fs::write(tmp.join("this"), b"self").unwrap();
    Agents {
        agent_dir: None,
        this: Some((tmp.join("this"), Own { target: own, glibc })),
        cache: Some(tmp.join("cache")),
        release_url: NOWHERE.into(),
    }
}

fn keys(plan: &[Candidate]) -> Vec<&str> {
    plan.iter().map(|c| c.key.as_str()).collect()
}

fn self_key(bytes: &[u8]) -> String {
    format!("self-{}", &sha256_hex(bytes)[..16])
}

#[test]
fn another_platform_gets_the_release() {
    let tmp = tempfile::tempdir().unwrap();
    let a = agents(tmp.path(), MUSL, None);
    let plan = a.plan(ARM).unwrap();
    assert_eq!(keys(&plan), [format!("{VERSION}-{ARM}")]);
    assert_eq!(plan[0].from, Origin::Release);
    assert_eq!(plan[0].size(), None);
    assert!(a.load(ARM, &plan[0]).is_err(), "nowhere to download from");

    let cached = tmp.path().join(format!("cache/{VERSION}-{ARM}/dfdu"));
    store(&cached, b"released").unwrap();
    let plan = a.plan(ARM).unwrap();
    assert_eq!(plan[0].from, Origin::Cached(cached));
    assert_eq!(plan[0].size(), Some(8));
    let (key, bin) = a.load(ARM, &plan[0]).unwrap();
    assert_eq!(
        (key.as_str(), bin.as_slice()),
        (plan[0].key.as_str(), &b"released"[..])
    );
}

#[test]
fn the_agent_dir_comes_first() {
    let tmp = tempfile::tempdir().unwrap();
    let mut a = agents(tmp.path(), MUSL, None);
    let dir = tmp.path().join("agents");
    std::fs::create_dir_all(dir.join(MUSL)).unwrap();
    std::fs::write(dir.join(MUSL).join("dfdu"), b"from dir").unwrap();
    a.agent_dir = Some(dir);
    let plan = a.plan(MUSL).unwrap();
    assert_eq!(
        keys(&plan),
        [format!("dir-{}", &sha256_hex(b"from dir")[..16])]
    );
    assert!(!plan[0].is_this());
}

#[test]
fn a_release_build_sends_itself() {
    let tmp = tempfile::tempdir().unwrap();
    let a = agents(tmp.path(), MUSL, None);
    store(
        &tmp.path().join(format!("cache/{VERSION}-{MUSL}/dfdu")),
        b"x",
    )
    .unwrap();
    let plan = a.plan(MUSL).unwrap();
    assert_eq!(keys(&plan), [self_key(b"self")]);
    assert!(plan[0].is_this());
    // Keyed by what is sent, should it have changed (a rebuild) since the plan.
    std::fs::write(tmp.path().join("this"), b"rebuilt").unwrap();
    assert_eq!(a.load(MUSL, &plan[0]).unwrap().0, self_key(b"rebuilt"));
}

#[test]
fn a_glibc_build_prefers_the_release_and_falls_back_to_itself() {
    let tmp = tempfile::tempdir().unwrap();
    let a = agents(tmp.path(), MUSL, Some("x86_64-unknown-linux-gnu"));
    let plan = a.plan(MUSL).unwrap();
    assert_eq!(
        keys(&plan),
        [format!("{VERSION}-{MUSL}"), self_key(b"self")]
    );
    assert_eq!(plan[0].from, Origin::Release);
    assert_eq!(plan[1].built_for, "x86_64-unknown-linux-gnu");
    assert!(plan[1].is_this());
}

#[test]
fn store_leaves_nothing_behind_on_failure() {
    let tmp = tempfile::tempdir().unwrap();
    // A directory where the file should go: the rename fails.
    let path = tmp.path().join("d/dfdu");
    std::fs::create_dir_all(path.join("x")).unwrap();
    assert!(store(&path, b"bin").is_err());
    let left: Vec<_> = std::fs::read_dir(tmp.path().join("d"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(left, ["dfdu"]);
}

#[cfg(feature = "agent-download")]
fn tar_xz(files: &[(&str, &[u8])]) -> Vec<u8> {
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

#[cfg(feature = "agent-download")]
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

/// Serves `files` (path, body) over HTTP, one request per connection; the base URL.
#[cfg(feature = "agent-download")]
fn serve(files: Vec<(String, Vec<u8>)>) -> String {
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

#[cfg(feature = "agent-download")]
#[test]
fn downloads_checks_and_caches_the_release() {
    let archive = tar_xz(&[(&format!("dfdu-{ARM}/dfdu"), b"agent bytes")]);
    let sum = format!("{}  dfdu-{ARM}.tar.xz\n", sha256_hex(&archive));
    let path = format!("/v{VERSION}/dfdu-{ARM}.tar.xz");
    let tmp = tempfile::tempdir().unwrap();
    let mut a = agents(tmp.path(), MUSL, None);
    a.release_url = serve(vec![
        (path.clone(), archive.clone()),
        (format!("{path}.sha256"), sum.into_bytes()),
    ]);
    let plan = a.plan(ARM).unwrap();
    assert_eq!(a.load(ARM, &plan[0]).unwrap().1, b"agent bytes");
    // Kept: the next run sends it without downloading.
    a.release_url = NOWHERE.into();
    let plan = a.plan(ARM).unwrap();
    assert!(matches!(plan[0].from, Origin::Cached(_)));
    assert_eq!(a.load(ARM, &plan[0]).unwrap().1, b"agent bytes");

    let other = tempfile::tempdir().unwrap();
    let mut a = agents(other.path(), MUSL, None);
    a.release_url = serve(vec![
        (path.clone(), archive),
        (
            format!("{path}.sha256"),
            format!("{}\n", "0".repeat(64)).into_bytes(),
        ),
    ]);
    let plan = a.plan(ARM).unwrap();
    let err = a.load(ARM, &plan[0]).unwrap_err().to_string();
    assert!(err.contains("does not match its published sha256"), "{err}");
    assert!(
        !other.path().join("cache").exists(),
        "a bad download is not kept"
    );

    a.release_url = serve(Vec::new());
    let err = a.load(ARM, &plan[0]).unwrap_err().to_string();
    assert!(
        err.contains(&format!("no dfdu {VERSION} release for {ARM}")),
        "{err}"
    );
}

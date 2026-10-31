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

#[test]
fn store_sweeps_the_temp_of_a_writer_that_died() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("d/dfdu");
    std::fs::create_dir_all(tmp.path().join("d")).unwrap();
    std::fs::write(tmp.path().join("d/.dfdu.4294967295.tmp"), b"cut sh").unwrap();
    store(&path, b"bin").unwrap();
    let left: Vec<_> = std::fs::read_dir(tmp.path().join("d"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(left, ["dfdu"]);
    assert_eq!(std::fs::read(&path).unwrap(), b"bin");
}

#[cfg(feature = "agent-download")]
#[test]
fn a_downloaded_release_is_kept_and_a_bad_one_is_not() {
    use super::release::tests::{published, serve};
    let tmp = tempfile::tempdir().unwrap();
    let mut a = agents(tmp.path(), MUSL, None);
    a.release_url = serve(published(ARM, b"agent bytes", None));
    let plan = a.plan(ARM).unwrap();
    assert_eq!(a.load(ARM, &plan[0]).unwrap().1, b"agent bytes");
    // Kept: the next run sends it without downloading.
    a.release_url = NOWHERE.into();
    let plan = a.plan(ARM).unwrap();
    assert!(matches!(plan[0].from, Origin::Cached(_)));
    assert_eq!(a.load(ARM, &plan[0]).unwrap().1, b"agent bytes");

    let other = tempfile::tempdir().unwrap();
    let mut a = agents(other.path(), MUSL, None);
    a.release_url = serve(published(ARM, b"agent bytes", Some(&"0".repeat(64))));
    let plan = a.plan(ARM).unwrap();
    assert!(a.load(ARM, &plan[0]).is_err());
    assert!(
        !other.path().join("cache").exists(),
        "a bad download is not kept"
    );
}

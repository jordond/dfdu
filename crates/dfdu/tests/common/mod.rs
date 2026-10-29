//! Helpers the integration tests share: running the `dfdu` binary and compiling the published
//! JSON schemas.

// Each test binary compiles this module and uses only some of it.
#![allow(dead_code)]

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value;

/// The `dfdu` binary, with stdin closed.
pub fn bin() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_dfdu"));
    c.stdin(Stdio::null());
    c
}

/// The `dfdu` binary with its state (history, journals) in `state`, and its scan cache below
/// it.
pub fn bin_in(state: &Path) -> Command {
    let mut c = bin();
    c.env("DFDU_STATE_DIR", state);
    c.env("DFDU_CACHE_DIR", state.join("cache"));
    c
}

/// The state directory the tests that do not keep their own share, so scans are not recorded
/// in the user's history.
pub fn shared_state() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("state")
}

/// Runs `dfdu args` with the shared state directory.
pub fn dfdu<S: AsRef<OsStr>>(args: &[S]) -> Output {
    dfdu_in(&shared_state(), args)
}

/// Runs `dfdu args` with its state in `state`.
pub fn dfdu_in<S: AsRef<OsStr>>(state: &Path, args: &[S]) -> Output {
    bin_in(state).args(args).output().unwrap()
}

/// The stdout of a run that must have succeeded.
pub fn ok(o: Output) -> String {
    assert!(
        o.status.success(),
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}

/// Base URI of the published schemas' `$id`s.
pub const SCHEMA_BASE: &str = "https://github.com/jordond/dfdu/schema/";

/// The schemas in `schema/` named in `load` (those `compile` refers to included), and the
/// indexes of the ones named in `compile`, in order.
pub fn schemas(load: &[&str], compile: &[&str]) -> (boon::Schemas, Vec<boon::SchemaIndex>) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schema");
    let mut c = boon::Compiler::new();
    for s in load {
        let v: Value = serde_json::from_slice(&fs::read(dir.join(s)).unwrap()).unwrap();
        assert_eq!(v["$id"], format!("{SCHEMA_BASE}{s}"));
        c.add_resource(&format!("{SCHEMA_BASE}{s}"), v).unwrap();
    }
    let mut schemas = boon::Schemas::new();
    let ix = compile
        .iter()
        .map(|s| {
            c.compile(&format!("{SCHEMA_BASE}{s}"), &mut schemas)
                .unwrap()
        })
        .collect();
    (schemas, ix)
}

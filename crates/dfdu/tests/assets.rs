//! The man pages and shell completions in `assets/`, which release archives and packages ship,
//! match what `dfdu man` and `dfdu completions` write. `DFDU_UPDATE_ASSETS=1` rewrites them.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const SHELLS: &[(&str, &str)] = &[
    ("bash", "dfdu.bash"),
    ("zsh", "_dfdu"),
    ("fish", "dfdu.fish"),
    ("elvish", "dfdu.elv"),
    ("powershell", "_dfdu.ps1"),
];

fn assets() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets")
}

fn dfdu(args: &[&str]) -> Vec<u8> {
    let o = Command::new(env!("CARGO_BIN_EXE_dfdu"))
        .args(args)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    o.stdout
}

/// The files of `dir` by name.
fn files(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .map(|e| {
            let e = e.unwrap();
            (
                e.file_name().into_string().unwrap(),
                fs::read(e.path()).unwrap(),
            )
        })
        .collect()
}

#[test]
fn assets_are_current() {
    let tmp = tempfile::tempdir().unwrap();
    let man = tmp.path().join("man");
    dfdu(&["man", man.to_str().unwrap()]);
    let comp = tmp.path().join("completions");
    fs::create_dir_all(&comp).unwrap();
    for (shell, name) in SHELLS {
        fs::write(comp.join(name), dfdu(&["completions", shell])).unwrap();
    }
    let update = std::env::var_os("DFDU_UPDATE_ASSETS").is_some();
    for dir in ["man", "completions"] {
        let want = files(&tmp.path().join(dir));
        let at = assets().join(dir);
        if update {
            let _ = fs::remove_dir_all(&at);
            fs::create_dir_all(&at).unwrap();
            for (name, data) in &want {
                fs::write(at.join(name), data).unwrap();
            }
            continue;
        }
        let have = files(&at);
        let names = |m: &BTreeMap<String, Vec<u8>>| m.keys().cloned().collect::<Vec<_>>();
        assert_eq!(
            names(&have),
            names(&want),
            "assets/{dir}: run DFDU_UPDATE_ASSETS=1 cargo test -p dfdu --test assets"
        );
        for (name, data) in &want {
            assert!(
                have[name] == *data,
                "assets/{dir}/{name} is stale: run DFDU_UPDATE_ASSETS=1 cargo test -p dfdu --test assets"
            );
        }
    }
}

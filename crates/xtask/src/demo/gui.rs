//! `cargo xtask demo --gui`: takes the desktop app's screenshots listed in demo/gui/shots, in
//! one container from demo/gui/Dockerfile, against the home fixture mounted by dfdu-demofs
//! (demo/README.md). dfdu-gui scans the fixture live, as the TUI recordings do, so the cleanup
//! rules and categories show; an imported scan would be read-only and unclassified.

use std::fs;
use std::process::Command;

use anyhow::{Context, bail};

use super::{DemoArgs, binaries, chosen};
use crate::cmd::{need, run};

const IMAGE: &str = "dfdu-demo-gui";
/// The list of screenshots: `NAME THEME KEYS` lines.
const SHOTS: &str = "demo/gui/shots";
/// Where the screenshots are copied to.
const ASSETS: &str = "assets/gui";
/// The binaries the screenshots need.
const BINS: [&str; 2] = ["dfdu-gui", "dfdu-demofs"];

/// The names of the shots listed in `text` (the contents of [`SHOTS`]), in order.
fn names(text: &str) -> anyhow::Result<Vec<String>> {
    let mut names: Vec<String> = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let at = || format!("{SHOTS}:{}", i + 1);
        let fields: Vec<&str> = line.split_whitespace().collect();
        let [name, theme, _keys] = fields[..] else {
            bail!("{}: want NAME THEME KEYS, got {line:?}", at());
        };
        if !matches!(theme, "dark" | "light") {
            bail!("{}: theme {theme:?} is not dark or light", at());
        }
        if names.iter().any(|n| n == name) {
            bail!("{}: {name:?} is listed twice", at());
        }
        names.push(name.to_owned());
    }
    Ok(names)
}

pub(super) fn shots(a: &DemoArgs) -> anyhow::Result<()> {
    let text = fs::read_to_string(SHOTS).with_context(|| format!("reading {SHOTS}"))?;
    let all = names(&text)?;
    if a.list {
        for s in &all {
            println!("{s}");
        }
        return Ok(());
    }
    let chosen = chosen(&a.tapes, &all, "shot", SHOTS)?;
    need("docker")?;
    let bins = binaries(a, &BINS, true)?;
    run(Command::new("docker").args(["build", "-q", "-t", IMAGE, "demo/gui"]))?;
    fs::create_dir_all(ASSETS)?;

    // A container left by an interrupted run.
    let _ = Command::new("docker").args(["rm", "-f", IMAGE]).output();
    run(Command::new("docker")
        .args(["create", "--privileged", "--name", IMAGE, IMAGE])
        .args(&chosen))?;
    let result = (|| {
        let cp = |from: String, to: String| run(Command::new("docker").args(["cp", &from, &to]));
        for b in BINS {
            cp(
                bins.join(b).display().to_string(),
                format!("{IMAGE}:/opt/dfdu/bin/{b}"),
            )?;
        }
        cp("demo/gui".into(), format!("{IMAGE}:/work/gui"))?;
        cp("demo/fixtures".into(), format!("{IMAGE}:/work/fixtures"))?;
        run(Command::new("docker").args(["start", "-a", IMAGE]))?;
        cp(format!("{IMAGE}:/work/out/."), ASSETS.to_owned())
    })();
    let _ = Command::new("docker").args(["rm", "-f", IMAGE]).output();
    result?;
    eprintln!("took {} into {ASSETS}/", chosen.join(", "));
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn the_shots_file_lists_names_in_order() {
        let text = fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(SHOTS),
        )
        .unwrap();
        let all = names(&text).unwrap();
        assert!(all.contains(&"cleanup".to_owned()), "{all:?}");
        assert!(all.iter().all(|n| !n.starts_with('#')));
    }

    #[test]
    fn comments_and_blank_lines_are_skipped() {
        let all =
            names("# a comment\n\nmap dark 2\n  # indented\ncleanup-light light 3d\n").unwrap();
        assert_eq!(all, ["map", "cleanup-light"]);
    }

    #[test]
    fn a_bad_line_names_its_place() {
        let e = names("map dark 2\nmap\n").unwrap_err().to_string();
        assert!(e.contains("demo/gui/shots:2"), "{e}");
        let e = names("map blue 2\n").unwrap_err().to_string();
        assert!(e.contains("not dark or light"), "{e}");
        let e = names("map dark 2\nmap light 2\n").unwrap_err().to_string();
        assert!(e.contains("listed twice"), "{e}");
        let e = names("map dark 2 extra\n").unwrap_err().to_string();
        assert!(e.contains("want NAME THEME KEYS"), "{e}");
    }
}

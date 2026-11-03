//! `cargo xtask demo`: records the tapes in demo/tapes with VHS, each in its own container
//! from demo/Dockerfile, against a fixture mounted by dfdu-demofs (demo/README.md). With
//! `--gui` it takes the desktop app's screenshots instead ([`gui`]).
//!
//! The binaries are built for Linux in the `rust` image (or taken from `--bin-dir`) and copied
//! into each container with the tapes and fixtures; nothing is bind-mounted into a recording,
//! so its disk list shows no host directory, and Docker may be remote (`DOCKER_HOST`).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};
use clap::{Args, ValueEnum};

use crate::cmd::{docker_run, need, run};

mod gui;

const IMAGE: &str = "dfdu-demo";
/// Where the recordings are copied to.
const ASSETS: &str = "assets/demos";
/// The binaries a recording needs.
const BINS: [&str; 2] = ["dfdu", "dfdu-demofs"];
/// Where the Linux binaries are put when built here.
const BUILT: &str = "demo/out/bin";

#[derive(Args)]
pub(crate) struct DemoArgs {
    /// The tapes to record (names in demo/tapes), or with --gui the screenshots to take (names
    /// in demo/gui/shots); every one when left out.
    tapes: Vec<String>,
    /// List the tapes, or with --gui the screenshots.
    #[arg(long)]
    list: bool,
    /// Take screenshots of the desktop app (demo/gui/shots) into assets/gui instead of
    /// recording the tapes.
    #[arg(long, conflicts_with = "formats")]
    gui: bool,
    /// Take the Linux binaries (dfdu and dfdu-demofs, or with --gui dfdu-gui and dfdu-demofs)
    /// from this directory instead of building them.
    #[arg(long)]
    bin_dir: Option<PathBuf>,
    /// The image to build the binaries in.
    #[arg(long, default_value = "rust:1.99")]
    build_image: String,
    /// What to record each tape as; repeat for several (`--format gif --format mp4`). The tapes'
    /// own `Output` lines name the GIF; other formats replace it.
    #[arg(long = "format", value_enum, default_values_t = [Format::Gif])]
    formats: Vec<Format>,
}

/// VHS's output formats. The videos are a fraction of the GIFs' size; dfdu.app plays them when
/// they are in assets/demos and falls back to the GIF.
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum Format {
    Gif,
    Mp4,
    Webm,
}

impl Format {
    fn ext(self) -> &'static str {
        match self {
            Format::Gif => "gif",
            Format::Mp4 => "mp4",
            Format::Webm => "webm",
        }
    }
}

/// Where the tapes are.
const TAPES: &str = "demo/tapes";

/// Names of the tapes in `dir`, sorted; `_`-prefixed ones are included by the others.
fn tapes(dir: &Path) -> anyhow::Result<Vec<String>> {
    let mut names: Vec<String> = fs::read_dir(dir)?
        .filter_map(|e| {
            let name = e.ok()?.file_name().into_string().ok()?;
            let stem = name.strip_suffix(".tape")?;
            (!stem.starts_with('_')).then(|| stem.to_owned())
        })
        .collect();
    names.sort();
    Ok(names)
}

/// The `kind`s (tapes, shots) `asked` for, of `all` (listed in `place`): every one when none
/// is named.
fn chosen(
    asked: &[String],
    all: &[String],
    kind: &str,
    place: &str,
) -> anyhow::Result<Vec<String>> {
    if let Some(t) = asked.iter().find(|t| !all.contains(t)) {
        bail!("no {kind} {t:?} in {place} (--list names them)");
    }
    Ok(if asked.is_empty() {
        all.to_vec()
    } else {
        asked.to_vec()
    })
}

/// The extensions of `formats` for VHS, each once, comma-separated.
fn extensions(formats: &[Format]) -> String {
    let mut exts: Vec<&str> = formats.iter().map(|f| f.ext()).collect();
    exts.sort_unstable();
    exts.dedup();
    exts.join(",")
}

/// The Linux binaries `bins`: from `--bin-dir`, or built (with the GUI's when `gui`).
fn binaries(a: &DemoArgs, bins: &[&str], gui: bool) -> anyhow::Result<PathBuf> {
    let dir = match &a.bin_dir {
        Some(d) => d.clone(),
        // The build bind-mounts this checkout, which a remote Docker does not have.
        None if std::env::var_os("DOCKER_HOST").is_some() => {
            bail!("with DOCKER_HOST set, build the Linux binaries elsewhere and pass --bin-dir")
        }
        None => build(&a.build_image, gui)?,
    };
    for b in bins {
        if !dir.join(b).is_file() {
            bail!("no {b} in {}", dir.display());
        }
    }
    Ok(dir)
}

pub(crate) fn demo(a: &DemoArgs) -> anyhow::Result<()> {
    if a.gui {
        return gui::shots(a);
    }
    let all = tapes(Path::new(TAPES))?;
    if a.list {
        for t in &all {
            println!("{t}");
        }
        return Ok(());
    }
    let chosen = chosen(&a.tapes, &all, "tape", TAPES)?;
    need("docker")?;
    let bins = binaries(a, &BINS, false)?;
    run(Command::new("docker").args(["build", "-q", "-t", IMAGE, "demo"]))?;
    fs::create_dir_all(ASSETS)?;
    let formats = extensions(&a.formats);
    for t in &chosen {
        record(t, &bins, &formats).with_context(|| format!("recording {t}"))?;
    }
    eprintln!("recorded {} as {formats} into {ASSETS}/", chosen.join(", "));
    Ok(())
}

/// Builds dfdu and dfdu-demofs for Linux (glibc, so libfaketime can set their clock), and
/// dfdu-gui when `gui`, and copies them to [`BUILT`]. dfdu-gui needs no system library to build:
/// it loads X11, Wayland, GL and Vulkan when it starts.
fn build(image: &str, gui: bool) -> anyhow::Result<PathBuf> {
    let (pkg, bin) = if gui {
        (" -p dfdu-gui", " /target/release/dfdu-gui")
    } else {
        ("", "")
    };
    let script = format!(
        "cargo build --locked --release -p dfdu -p dfdu-demofs{pkg} \
         && mkdir -p {BUILT} && cp /target/release/dfdu /target/release/dfdu-demofs{bin} {BUILT}/"
    );
    run(docker_run(image, "dfdu-target", false).args(["bash", "-c", &script]))?;
    Ok(PathBuf::from(BUILT))
}

/// Records tape `name` in a new container as `formats` (extensions, comma-separated) and
/// copies what it wrote to [`ASSETS`].
fn record(name: &str, bins: &Path, formats: &str) -> anyhow::Result<()> {
    let container = format!("dfdu-demo-{name}");
    // A container left by an interrupted run.
    let _ = Command::new("docker")
        .args(["rm", "-f", &container])
        .output();
    run(Command::new("docker").args([
        "create",
        "--privileged",
        "--hostname",
        "devbox",
        "--name",
        &container,
        "--env",
        &format!("DEMO_FORMATS={formats}"),
        IMAGE,
        name,
    ]))?;
    let result = (|| {
        let cp = |from: String, to: String| run(Command::new("docker").args(["cp", &from, &to]));
        for b in BINS {
            cp(
                bins.join(b).display().to_string(),
                format!("{container}:/opt/dfdu/bin/{b}"),
            )?;
        }
        for part in ["run.sh", "env.sh", "tapes", "fixtures"] {
            cp(format!("demo/{part}"), format!("{container}:/work/{part}"))?;
        }
        run(Command::new("docker").args(["start", "-a", &container]))?;
        cp(format!("{container}:/work/out/."), ASSETS.to_owned())
    })();
    let _ = Command::new("docker")
        .args(["rm", "-f", &container])
        .output();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(n: &[&str]) -> Vec<String> {
        n.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn the_tapes_are_listed_sorted_without_their_includes() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(TAPES);
        let all = tapes(&dir).unwrap();
        assert!(all.contains(&"browse".to_owned()), "{all:?}");
        assert!(
            all.iter()
                .all(|t| !t.starts_with('_') && Path::new(t).extension().is_none())
        );
        assert!(all.is_sorted());
        assert!(tapes(&dir.join("no-such-dir")).is_err());
    }

    #[test]
    fn no_tape_named_is_every_tape() {
        let all = names(&["a", "b", "c"]);
        assert_eq!(chosen(&[], &all, "tape", TAPES).unwrap(), all);
        assert_eq!(
            chosen(&names(&["c", "a"]), &all, "tape", TAPES).unwrap(),
            ["c", "a"]
        );
        let e = chosen(&names(&["a", "z"]), &all, "tape", TAPES)
            .unwrap_err()
            .to_string();
        assert!(e.contains("no tape \"z\" in demo/tapes"), "{e}");
    }

    #[test]
    fn formats_are_named_once_each() {
        assert_eq!(extensions(&[Format::Gif]), "gif");
        assert_eq!(
            extensions(&[Format::Webm, Format::Gif, Format::Mp4, Format::Gif]),
            "gif,mp4,webm"
        );
        assert_eq!(extensions(&[]), "");
    }
}

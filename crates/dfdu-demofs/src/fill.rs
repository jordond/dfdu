//! Bulk content for a fixture directory: `fill = { files, size, style, seed }` makes `files`
//! files adding up to `size`, named and laid out like what the style stands for, with sizes
//! and ages drawn from `seed`. The same fill always makes the same files.

use anyhow::Context as _;
use serde::Deserialize;

use crate::fixture::{Amount, parse_age};
use crate::tree::{Kind, Tree, mix, seed_of};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fill {
    pub files: usize,
    pub size: Amount,
    #[serde(default)]
    pub seed: u64,
    #[serde(default)]
    pub style: Style,
    /// Files are up to this much older than the directory (default `30d`).
    pub spread: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Style {
    /// `file-0001.dat`.
    #[default]
    Generic,
    /// Packages of a `node_modules`.
    Npm,
    /// A Cargo `target/<profile>` directory.
    Cargo,
    /// A Python `site-packages`.
    Python,
    /// A camera roll.
    Photos,
    /// Content-addressed cache blobs.
    Cache,
}

const NPM: &[&str] = &[
    "react",
    "react-dom",
    "typescript",
    "esbuild",
    "webpack",
    "lodash",
    "@babel/core",
    "@babel/parser",
    "eslint",
    "prettier",
    "rxjs",
    "date-fns",
    "zod",
    "axios",
    "vite",
    "rollup",
    "postcss",
    "tailwindcss",
    "terser",
    "core-js",
    "@types/node",
    "jest",
    "@swc/core",
    "sharp",
    "next",
    "three",
    "chart.js",
    "moment",
    "graphql",
    "undici",
];

const CRATES: &[&str] = &[
    "serde",
    "tokio",
    "syn",
    "regex",
    "clap",
    "hyper",
    "rand",
    "anyhow",
    "ratatui",
    "crossbeam",
    "rayon",
    "tracing",
    "reqwest",
    "rustls",
    "proc_macro2",
    "quote",
    "libc",
    "memchr",
    "bytes",
    "futures",
    "sqlx",
    "image",
    "zstd",
    "itertools",
    "gix",
];

const PYTHON: &[&str] = &[
    "numpy",
    "pandas",
    "scipy",
    "matplotlib",
    "torch",
    "requests",
    "urllib3",
    "jinja2",
    "pydantic",
    "sklearn",
    "PIL",
    "certifi",
    "yaml",
    "setuptools",
    "pip",
    "IPython",
];

/// Files of a fill, relative to its directory, with their sizes and ages.
fn layout(f: &Fill, rng: &mut u64) -> Vec<String> {
    let hex = |rng: &mut u64| format!("{:016x}", next(rng));
    (0..f.files)
        .map(|i| match f.style {
            Style::Generic => format!("file-{:04}.dat", i + 1),
            Style::Photos => {
                let ext = if i % 7 == 6 { "MOV" } else { "HEIC" };
                format!("IMG_{:04}.{ext}", 4000 + i)
            }
            Style::Cache => {
                let h = hex(rng);
                format!("{}/{}", &h[..2], &h[2..])
            }
            Style::Npm => {
                let pkg = NPM[i % NPM.len()];
                let pass = i / NPM.len();
                const FILES: &[&str] = &[
                    "package.json",
                    "index.js",
                    "dist/index.js",
                    "README.md",
                    "dist/index.min.js",
                    "LICENSE",
                    "dist/index.cjs",
                    "types/index.d.ts",
                ];
                FILES.get(pass).map_or_else(
                    || format!("{pkg}/lib/chunk-{pass}.js"),
                    |f| format!("{pkg}/{f}"),
                )
            }
            Style::Cargo => {
                let c = CRATES[i % CRATES.len()];
                let h = &hex(rng)[..16];
                match i / CRATES.len() % 4 {
                    0 => format!("deps/lib{c}-{h}.rlib"),
                    1 => format!("deps/lib{c}-{h}.rmeta"),
                    2 => format!("deps/{c}-{h}.d"),
                    _ => format!("incremental/{c}-{h}/s-{}/query-cache.bin", &h[..8]),
                }
            }
            Style::Python => {
                let p = PYTHON[i % PYTHON.len()];
                match i / PYTHON.len() {
                    0 => format!("{p}/__init__.py"),
                    1 => format!("{p}/_core.cpython-313-x86_64-linux-gnu.so"),
                    2 => format!("{p}-1.{}.0.dist-info/RECORD", i % 9),
                    k => format!("{p}/_mod{k}.py"),
                }
            }
        })
        .collect()
}

fn next(rng: &mut u64) -> u64 {
    *rng = mix(*rng);
    *rng
}

/// Adds the files of `f` to directory `dir` (at `path` in the fixture), made at `mtime`.
pub(crate) fn fill(
    tree: &mut Tree,
    dir: usize,
    f: &Fill,
    mtime: i64,
    path: &str,
) -> anyhow::Result<()> {
    let total = f.size.bytes()?;
    let spread = parse_age(f.spread.as_deref().unwrap_or("30d"))?.max(1);
    let mut rng = f.seed ^ seed_of(path.as_bytes());
    let names = layout(f, &mut rng);
    // Cubed weights: a few large files and many small ones, as real trees have.
    let weights: Vec<u64> = names
        .iter()
        .map(|_| {
            let w = next(&mut rng) % 1000 + 1;
            w * w * w
        })
        .collect();
    let sum: u64 = weights.iter().sum::<u64>().max(1);
    let mut left = total;
    for (k, (name, w)) in names.iter().zip(&weights).enumerate() {
        let size = if k + 1 == names.len() {
            left
        } else {
            u64::try_from(u128::from(total) * u128::from(*w) / u128::from(sum))
                .unwrap_or(0)
                .min(left)
        };
        left -= size;
        let age = i64::try_from(next(&mut rng) % u64::try_from(spread)?)?;
        let t = mtime - age;
        let (sub, file) = name.rsplit_once('/').unwrap_or(("", name));
        let d = if sub.is_empty() {
            dir
        } else {
            sub_dir(tree, dir, sub, t)?
        };
        let seed = seed_of(format!("{path}/{name}").as_bytes());
        tree.insert(d, file.as_bytes(), Kind::seeded(size, seed), t, 0o644)
            .map_err(|e| anyhow::anyhow!("{e:?}"))
            .with_context(|| format!("fill file {name:?}"))?;
    }
    Ok(())
}

/// The directory `rel` below `dir`, made when missing.
fn sub_dir(tree: &mut Tree, dir: usize, rel: &str, mtime: i64) -> anyhow::Result<usize> {
    let mut d = dir;
    for part in rel.split('/') {
        d = match tree.lookup(d, part.as_bytes()) {
            Ok(n) if tree.nodes[n].is_dir() => n,
            Ok(_) => anyhow::bail!("{part:?} in {rel:?} is not a directory"),
            Err(_) => tree
                .insert(d, part.as_bytes(), Kind::dir(), mtime, 0o755)
                .map_err(|e| anyhow::anyhow!("{e:?}"))?,
        };
    }
    Ok(d)
}

//! The fixture file: what the filesystem serves, read from TOML and built into a
//! [`Tree`](crate::Tree).
//!
//! ```toml
//! now = "2026-10-01T09:00:00Z"     # the time the recording runs at; ages count back from it
//! age = "30d"                      # age of entries that do not set one
//!
//! [disk]                           # what statfs answers
//! total = "1TB"
//! free = "212GB"
//!
//! [[entry]]
//! path = "Downloads/ubuntu.iso"    # parent directories are made as needed
//! size = "5.8GiB"
//! age = "200d"
//! content = "iso"                  # files with the same content read the same bytes
//!
//! [[entry]]
//! path = "dev/shop/node_modules"
//! fill = { files = 600, size = "1.4GiB", style = "npm", seed = 7 }
//! ```
//!
//! Entry keys: `path`, `kind` (`file`, `dir`, `symlink`; `dir` when `fill` is given), `size`,
//! `alloc` (bytes allocated, when not the size in 4 KiB blocks), `age`, `content`, `text` (the
//! file's bytes), `target` (a symlink's), `link` (a hard link to an earlier entry's path),
//! `mode`, `error` (`denied` or `io`), `delay` (a directory's listing opens that late) and
//! `fill`.
//!
//! `import` lists real paths copied in at the same place when the fixture is built, for a root
//! a shell can run in; [`Fixture::build_at`] builds one fixture inside another's tree.

mod build;
mod import;
mod values;

use anyhow::{Context as _, bail};
use serde::Deserialize;

pub(crate) use build::{insert, mkdir_p};
pub use values::{format_time, parse_age};
pub(crate) use values::{parse_duration, parse_size, parse_time};

use crate::Error;
use crate::fill::Fill;

#[derive(Debug)]
pub struct Fixture {
    /// When the recording runs: `YYYY-MM-DD`, `YYYY-MM-DDTHH:MM:SSZ` or with a space.
    pub now: String,
    /// Age of entries that set none.
    pub age: String,
    pub uid: u32,
    pub gid: u32,
    pub disk: Disk,
    /// Real files, symlinks and directories (with what is below them) copied in at the same
    /// path when the fixture is built, for a root a shell can run in.
    pub import: Vec<String>,
    pub entries: Vec<Entry>,
}

/// A fixture file as written: what it sets, over the fixture it `extends` if any.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Layer {
    /// Another fixture file, relative to this one: its keys are kept where this one sets none,
    /// and its entries where this one does not `remove` or give the same `path`.
    extends: Option<String>,
    now: Option<String>,
    age: Option<String>,
    uid: Option<u32>,
    gid: Option<u32>,
    disk: Option<Disk>,
    #[serde(default)]
    import: Vec<String>,
    /// Inherited entries to leave out, with every inherited entry below them.
    #[serde(default)]
    remove: Vec<String>,
    #[serde(default, rename = "entry")]
    entries: Vec<Entry>,
}

/// How many fixtures may extend one another in a chain, which catches loops.
const MAX_EXTENDS: usize = 8;

/// The filesystem as `statfs` and the mount table show it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Disk {
    pub total: Amount,
    /// Free space with the fixture's files on it; deleting adds to it.
    pub free: Amount,
    /// The mount's source, as the mount table lists it.
    pub device: String,
}

impl Default for Disk {
    fn default() -> Disk {
        Disk {
            total: Amount::Text("1TB".into()),
            free: Amount::Text("200GB".into()),
            device: "dfdu-demofs".into(),
        }
    }
}

/// A size: bytes, or text such as `1.4GiB` or `500MB`.
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum Amount {
    Bytes(u64),
    Text(String),
}

impl Amount {
    /// The size in bytes, or an error for text that is not a size.
    pub(crate) fn bytes(&self) -> anyhow::Result<u64> {
        match self {
            Amount::Bytes(b) => Ok(*b),
            Amount::Text(t) => parse_size(t),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FaultName {
    Denied,
    Io,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub path: String,
    pub kind: Option<EntryKind>,
    pub size: Option<Amount>,
    pub alloc: Option<Amount>,
    pub age: Option<String>,
    pub content: Option<String>,
    pub text: Option<String>,
    pub target: Option<String>,
    pub link: Option<String>,
    pub mode: Option<u32>,
    pub error: Option<FaultName>,
    pub delay: Option<String>,
    pub fill: Option<Fill>,
}

impl Fixture {
    /// Reads fixture text.
    ///
    /// # Errors
    ///
    /// The text is not a fixture.
    pub fn parse(text: &str) -> Result<Fixture, Error> {
        Ok(Fixture::parse_over(text, |name| {
            bail!("extends {name:?}: only a fixture read from a file can extend another")
        })?)
    }

    /// Reads the fixture file at `path`, and the ones it extends.
    ///
    /// # Errors
    ///
    /// A file cannot be read or is not a fixture, or the files extend one another in a loop.
    pub fn load(path: &std::path::Path) -> Result<Fixture, Error> {
        Ok(Fixture::load_chain(path, 0)?)
    }

    fn load_chain(path: &std::path::Path, depth: usize) -> anyhow::Result<Fixture> {
        if depth > MAX_EXTENDS {
            bail!("more than {MAX_EXTENDS} fixtures extend one another");
        }
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Fixture::parse_over(&text, |name| {
            let dir = path.parent().unwrap_or(std::path::Path::new(""));
            Fixture::load_chain(&dir.join(name), depth + 1)
        })
        .with_context(|| format!("in {}", path.display()))
    }

    /// Reads fixture text; `base` gives the fixture named by its `extends`.
    pub(crate) fn parse_over(
        text: &str,
        base: impl FnOnce(&str) -> anyhow::Result<Fixture>,
    ) -> anyhow::Result<Fixture> {
        let l: Layer = toml::from_str(text)?;
        let mut f = match &l.extends {
            Some(name) => base(name)?,
            None => Fixture {
                now: String::new(),
                age: "30d".into(),
                uid: 1000,
                gid: 1000,
                disk: Disk::default(),
                import: vec![],
                entries: vec![],
            },
        };
        if let Some(now) = l.now {
            f.now = now;
        } else if l.extends.is_none() {
            bail!("now is missing");
        }
        f.age = l.age.unwrap_or(f.age);
        f.uid = l.uid.unwrap_or(f.uid);
        f.gid = l.gid.unwrap_or(f.gid);
        f.disk = l.disk.unwrap_or(f.disk);
        f.import.extend(l.import);
        for r in &l.remove {
            let below = format!("{r}/");
            let before = f.entries.len();
            f.entries
                .retain(|e| e.path != *r && !e.path.starts_with(&below));
            if f.entries.len() == before {
                bail!("remove {r:?}: no such entry");
            }
        }
        let inherited = f.entries.len();
        for e in l.entries {
            match f.entries[..inherited].iter_mut().find(|x| x.path == e.path) {
                Some(slot) => *slot = e,
                None => f.entries.push(e),
            }
        }
        f.now()?;
        Ok(f)
    }

    /// [`Fixture::now`] in seconds since the epoch.
    ///
    /// # Errors
    ///
    /// `now` is not a time.
    pub fn now(&self) -> Result<i64, Error> {
        Ok(parse_time(&self.now).with_context(|| format!("now = {:?}", self.now))?)
    }
}

#[cfg(test)]
mod tests;

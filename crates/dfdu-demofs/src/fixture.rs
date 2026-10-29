//! The fixture file: what the filesystem serves, read from TOML and built into a [`Tree`].
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

use std::time::Duration;

use anyhow::{Context as _, anyhow, bail};
use serde::Deserialize;

use crate::fill::{self, Fill};
use crate::tree::{Data, Fault, Kind, Tree, seed_of};

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
    /// The size in bytes.
    ///
    /// # Errors
    ///
    /// Text that is not a size.
    pub fn bytes(&self) -> anyhow::Result<u64> {
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
    pub fn parse(text: &str) -> anyhow::Result<Fixture> {
        Fixture::parse_over(text, |name| {
            bail!("extends {name:?}: only a fixture read from a file can extend another")
        })
    }

    /// Reads the fixture file at `path`, and the ones it extends.
    ///
    /// # Errors
    ///
    /// A file cannot be read or is not a fixture, or the files extend one another in a loop.
    pub fn load(path: &std::path::Path) -> anyhow::Result<Fixture> {
        Fixture::load_chain(path, 0)
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
    ///
    /// # Errors
    ///
    /// The text is not a fixture, or `base` fails.
    pub fn parse_over(
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
    pub fn now(&self) -> anyhow::Result<i64> {
        parse_time(&self.now).with_context(|| format!("now = {:?}", self.now))
    }

    /// The tree the fixture describes.
    ///
    /// # Errors
    ///
    /// An entry is malformed, its path runs through a file, it is given twice, or it links to
    /// a path that is not an earlier file; an import cannot be read.
    pub fn build(&self) -> anyhow::Result<Tree> {
        let mut tree = Tree::new(0, (self.uid, self.gid));
        self.build_at(&mut tree, "")?;
        Ok(tree)
    }

    /// Builds the fixture into `tree` with its root at the directory `at` (made with its
    /// parents when missing), owned by the fixture's uid and gid. Imports come first, then the
    /// entries.
    ///
    /// # Errors
    ///
    /// As [`Fixture::build`], or `at` runs through a file.
    pub fn build_at(&self, tree: &mut Tree, at: &str) -> anyhow::Result<()> {
        let now = self.now()?;
        let default_mtime = now - parse_age(&self.age)?;
        tree.owner = (self.uid, self.gid);
        let root = mkdir_p(tree, at, default_mtime)?;
        let node = &mut tree.nodes[root];
        node.mtime = default_mtime;
        (node.uid, node.gid) = tree.owner;
        let under = |p: &str| {
            let p = p.trim_start_matches('/');
            if at.is_empty() {
                p.to_owned()
            } else {
                format!("{at}/{p}")
            }
        };
        for from in &self.import {
            let dir = mkdir_p(tree, &parent_of(&under(from)), default_mtime)?;
            import(tree, dir, std::path::Path::new(from), default_mtime)
                .with_context(|| format!("import {from:?}"))?;
        }
        for e in &self.entries {
            add(tree, e, &under, now, default_mtime)
                .with_context(|| format!("entry {:?}", e.path))?;
        }
        Ok(())
    }
}

fn parent_of(path: &str) -> String {
    path.rsplit_once('/')
        .map_or(String::new(), |(p, _)| p.to_owned())
}

/// Copies the file, symlink or directory at `from` into `dir` under its own name, with its
/// permission bits; a directory with everything below it, in name order.
#[cfg(unix)]
fn import(tree: &mut Tree, dir: usize, from: &std::path::Path, mtime: i64) -> anyhow::Result<()> {
    use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
    use std::os::unix::fs::PermissionsExt as _;
    let name = from
        .file_name()
        .with_context(|| format!("{} has no name", from.display()))?;
    let meta = std::fs::symlink_metadata(from).with_context(|| from.display().to_string())?;
    let mode = meta.permissions().mode() & 0o7777;
    let kind = if meta.is_symlink() {
        Kind::Symlink(std::fs::read_link(from)?.into_os_string().into_vec())
    } else if meta.is_dir() {
        Kind::dir()
    } else if meta.is_file() {
        let b = std::fs::read(from).with_context(|| from.display().to_string())?;
        Kind::File {
            size: b.len() as u64,
            alloc: None,
            data: Data::Bytes(b),
        }
    } else {
        bail!("{} is not a file, directory or symlink", from.display());
    };
    let is_dir = matches!(kind, Kind::Dir { .. });
    let n = tree
        .insert(dir, name.as_bytes(), kind, mtime, mode)
        .map_err(|e| anyhow!("{}: {e:?}", from.display()))?;
    if is_dir {
        let mut names: Vec<_> = std::fs::read_dir(from)?
            .map(|e| e.map(|e| e.path()))
            .collect::<Result<_, _>>()?;
        names.sort();
        for p in names {
            import(tree, n, &p, mtime)?;
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn import(_: &mut Tree, _: usize, _: &std::path::Path, _: i64) -> anyhow::Result<()> {
    bail!("imports need a Unix system")
}

fn add(
    tree: &mut Tree,
    e: &Entry,
    under: &impl Fn(&str) -> String,
    now: i64,
    default_mtime: i64,
) -> anyhow::Result<()> {
    let mtime = e
        .age
        .as_deref()
        .map_or(Ok(default_mtime), |a| parse_age(a).map(|a| now - a))?;
    let path = under(&e.path);
    let (parent, name) = path.rsplit_once('/').unwrap_or(("", &path));
    let dir = mkdir_p(tree, parent, mtime)?;
    if let Some(to) = &e.link {
        let target = tree
            .resolve(&under(to))
            .map_err(|_| anyhow!("link {to:?}: no such entry"))?;
        return tree
            .link(target, dir, name.as_bytes())
            .map_err(|err| anyhow!("link {to:?}: {err:?}"));
    }
    let kind = e.kind.unwrap_or(if e.fill.is_some() {
        EntryKind::Dir
    } else if e.target.is_some() {
        EntryKind::Symlink
    } else {
        EntryKind::File
    });
    let n = match kind {
        EntryKind::Dir => match tree.lookup(dir, name.as_bytes()) {
            // A directory made for an earlier entry's path: this entry sets its attributes.
            Ok(n) if tree.nodes[n].is_dir() => n,
            _ => insert(tree, dir, name, Kind::dir(), mtime, 0o755)?,
        },
        EntryKind::Symlink => {
            let target = e.target.as_deref().context("a symlink needs a target")?;
            let kind = Kind::Symlink(target.as_bytes().to_vec());
            insert(tree, dir, name, kind, mtime, 0o777)?
        }
        EntryKind::File => {
            let (size, data) = match (&e.text, &e.size) {
                (Some(t), _) => (t.len() as u64, Data::Bytes(t.as_bytes().to_vec())),
                (None, s) => {
                    let id = e.content.as_deref().unwrap_or(&e.path);
                    let size = s.as_ref().map_or(Ok(0), Amount::bytes)?;
                    let seed = seed_of(id.as_bytes());
                    (size, Data::Seeded { seed, len: size })
                }
            };
            let alloc = e.alloc.as_ref().map(Amount::bytes).transpose()?;
            insert(
                tree,
                dir,
                name,
                Kind::File { size, alloc, data },
                mtime,
                0o644,
            )?
        }
    };
    let node = &mut tree.nodes[n];
    node.mtime = mtime;
    if let Some(m) = e.mode {
        node.mode = m;
    }
    node.fault = e.error.as_ref().map(|f| match f {
        FaultName::Denied => Fault::Denied,
        FaultName::Io => Fault::Io,
    });
    if let Some(d) = &e.delay {
        node.delay = parse_duration(d)?;
    }
    if let Some(f) = &e.fill {
        fill::fill(tree, n, f, mtime, &e.path)?;
    }
    Ok(())
}

fn insert(
    tree: &mut Tree,
    dir: usize,
    name: &str,
    kind: Kind,
    mtime: i64,
    mode: u32,
) -> anyhow::Result<usize> {
    tree.insert(dir, name.as_bytes(), kind, mtime, mode)
        .map_err(|e| anyhow!("{e:?}"))
}

/// The directory at `path`, made with its parents when missing.
fn mkdir_p(tree: &mut Tree, path: &str, mtime: i64) -> anyhow::Result<usize> {
    let mut d = 0;
    for part in path.split('/').filter(|p| !p.is_empty()) {
        d = match tree.lookup(d, part.as_bytes()) {
            Ok(n) if tree.nodes[n].is_dir() => n,
            Ok(_) => bail!("{part:?} in {path:?} is not a directory"),
            Err(_) => insert(tree, d, part, Kind::dir(), mtime, 0o755)?,
        };
    }
    Ok(d)
}

/// Parses a size: a number with an optional fraction and unit (`B`, `KB`/`MB`/`GB`/`TB` in
/// powers of 1000, `KiB`/`MiB`/`GiB`/`TiB` in powers of 1024).
///
/// # Errors
///
/// Not a size.
pub fn parse_size(s: &str) -> anyhow::Result<u64> {
    let t = s.trim().replace('_', "");
    let split = t
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(t.len());
    let (num, unit) = t.split_at(split);
    let mult: u64 = match unit.trim() {
        "" | "B" => 1,
        "KB" => 1_000,
        "MB" => 1_000_000,
        "GB" => 1_000_000_000,
        "TB" => 1_000_000_000_000,
        "KiB" => 1 << 10,
        "MiB" => 1 << 20,
        "GiB" => 1 << 30,
        "TiB" => 1 << 40,
        u => bail!("size {s:?}: unknown unit {u:?}"),
    };
    let (whole, frac) = num.split_once('.').unwrap_or((num, ""));
    let whole: u64 = whole.parse().with_context(|| format!("size {s:?}"))?;
    let frac_val: u64 = if frac.is_empty() {
        0
    } else {
        frac.parse().with_context(|| format!("size {s:?}"))?
    };
    let den = 10u128.pow(u32::try_from(frac.len())?);
    let part = u128::from(frac_val) * u128::from(mult) / den;
    whole
        .checked_mul(mult)
        .and_then(|b| b.checked_add(u64::try_from(part).ok()?))
        .with_context(|| format!("size {s:?} is too large"))
}

/// Parses a duration: a whole number and `ms`, `s`, `m`, `h`, `d`, `w` or `y` (365 days).
///
/// # Errors
///
/// Not a duration.
pub fn parse_duration(s: &str) -> anyhow::Result<Duration> {
    let t = s.trim();
    let split = t.find(|c: char| !c.is_ascii_digit()).unwrap_or(t.len());
    let n: u64 = t[..split]
        .parse()
        .with_context(|| format!("duration {s:?}"))?;
    let secs = match &t[split..] {
        "ms" => return Ok(Duration::from_millis(n)),
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        "w" => 7 * 86_400,
        "y" => 365 * 86_400,
        u => bail!("duration {s:?}: unknown unit {u:?}"),
    };
    let secs = n
        .checked_mul(secs)
        .with_context(|| format!("duration {s:?} is too long"))?;
    Ok(Duration::from_secs(secs))
}

/// [`parse_duration`] in whole seconds.
///
/// # Errors
///
/// Not a duration.
pub fn parse_age(s: &str) -> anyhow::Result<i64> {
    Ok(i64::try_from(parse_duration(s)?.as_secs())?)
}

/// Parses `YYYY-MM-DD`, `YYYY-MM-DDTHH:MM:SS` or `YYYY-MM-DD HH:MM:SS`, UTC, with an optional
/// trailing `Z`, into seconds since the epoch.
///
/// # Errors
///
/// Not such a time.
pub fn parse_time(s: &str) -> anyhow::Result<i64> {
    let t = s.trim().trim_end_matches('Z');
    let (date, time) = t.split_once(['T', ' ']).unwrap_or((t, "00:00:00"));
    let num = |p: &str| p.parse::<i64>().with_context(|| format!("time {s:?}"));
    let d: Vec<&str> = date.split('-').collect();
    let h: Vec<&str> = time.split(':').collect();
    let ([y, mo, da], [hh, mm, ss]) = (d.as_slice(), h.as_slice()) else {
        bail!("time {s:?}: expected YYYY-MM-DD[THH:MM:SS]");
    };
    let (y, mo, da) = (num(y)?, num(mo)?, num(da)?);
    let (hh, mm, ss) = (num(hh)?, num(mm)?, num(ss)?);
    // A date that does not exist (February 31st) comes back as another one.
    let days = days_from_civil(y, mo, da);
    if !(1..=12).contains(&mo) || civil_from_days(days) != (y, mo, da) {
        bail!("time {s:?}: no such date");
    }
    if !(0..24).contains(&hh) || !(0..60).contains(&mm) || !(0..60).contains(&ss) {
        bail!("time {s:?}: no such time of day");
    }
    Ok(days * 86_400 + hh * 3600 + mm * 60 + ss)
}

/// `secs` as `YYYY-MM-DD HH:MM:SS` UTC, as libfaketime's `FAKETIME` takes it after an `@`.
#[must_use]
pub fn format_time(secs: i64) -> String {
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

//! Imports: real files, symlinks and directories copied into a fixture's tree, for a root a
//! shell can run in.

use anyhow::bail;

use crate::tree::Tree;

/// The paths an import names: itself, or with `*` in a component (any run of characters, not
/// `/`) every path that matches, in name order, so one list serves each architecture's
/// library paths. A pattern that matches nothing is an error, as a missing path is.
pub(super) fn expand(pattern: &str) -> anyhow::Result<Vec<String>> {
    if !pattern.contains('*') {
        return Ok(vec![pattern.to_owned()]);
    }
    let mut paths = vec![String::new()];
    for part in pattern.split('/').filter(|p| !p.is_empty()) {
        let mut next = vec![];
        for dir in &paths {
            if !part.contains('*') {
                next.push(format!("{dir}/{part}"));
                continue;
            }
            let Ok(entries) = std::fs::read_dir(if dir.is_empty() { "/" } else { dir }) else {
                continue;
            };
            let mut names: Vec<_> = entries
                .filter_map(|e| e.ok()?.file_name().into_string().ok())
                .filter(|n| matches_glob(part, n))
                .collect();
            names.sort();
            next.extend(names.into_iter().map(|n| format!("{dir}/{n}")));
        }
        paths = next;
    }
    paths.retain(|p| std::fs::symlink_metadata(p).is_ok());
    if paths.is_empty() {
        bail!("no path matches");
    }
    Ok(paths)
}

/// Whether `name` matches `pattern`, where `*` stands for any run of characters.
fn matches_glob(pattern: &str, name: &str) -> bool {
    let mut parts = pattern.split('*');
    let first = parts.next().unwrap_or_default();
    let Some(mut rest) = name.strip_prefix(first) else {
        return false;
    };
    let parts: Vec<_> = parts.collect();
    let Some((last, middle)) = parts.split_last() else {
        return rest.is_empty();
    };
    for p in middle {
        match rest.find(p) {
            Some(i) => rest = &rest[i + p.len()..],
            None => return false,
        }
    }
    rest.len() >= last.len() && rest.ends_with(last)
}

/// Copies the file, symlink or directory at `from` into `dir` under its own name, with its
/// permission bits; a directory with everything below it, in name order.
#[cfg(unix)]
pub(super) fn import(
    tree: &mut Tree,
    dir: usize,
    from: &std::path::Path,
    mtime: i64,
) -> anyhow::Result<()> {
    use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
    use std::os::unix::fs::PermissionsExt as _;

    use anyhow::{Context as _, anyhow};

    use crate::tree::Kind;

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
        Kind::stored(std::fs::read(from).with_context(|| from.display().to_string())?)
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
pub(super) fn import(_: &mut Tree, _: usize, _: &std::path::Path, _: i64) -> anyhow::Result<()> {
    bail!("imports need a Unix system")
}

#[cfg(test)]
mod tests;

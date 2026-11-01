//! The file type filter: shows only files of chosen types (video, audio, image...) or
//! extensions, everywhere below the current directory. While it is on, a snapshot counts only
//! the matching files ([`crate::snapshot::build_view`]), and a front end that lists the
//! directory's files by type and extension (a picker) asks for that breakdown with it. The
//! picker's rows ([`rows`]), what choosing one does ([`toggle`]) and turning the filter off
//! ([`clear`]) are here; the picker's input, cursor and scroll are the front end's.

use dfdu_tree::Totals;
use dfdu_tree::filetype::{Ext, FileType, TypeFilter};

use crate::state::SessionState;

/// Extensions listed at most, largest first; chosen ones are always listed.
pub const MAX_EXTS: usize = 300;

/// A row of the picker.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Row {
    /// Every file: choosing it clears the filter.
    All,
    Type(FileType),
    Ext(Ext),
}

/// Whether the filter is on.
pub fn active(s: &SessionState) -> bool {
    !s.types.is_empty()
}

/// Whether the snapshot was read with another filter, or lacks the breakdown the front end
/// shows (`breakdown`).
pub fn stale(s: &SessionState, breakdown: bool) -> bool {
    s.snap.types != s.types || (breakdown && s.snap.breakdown.is_none())
}

/// Turns the filter off, and says so; false when it was off.
pub fn clear(s: &mut SessionState) -> bool {
    if s.types.is_empty() {
        return false;
    }
    s.types = TypeFilter::default();
    s.status = Some("showing all files".into());
    true
}

/// The rows for `query`: every file, then the types and the extensions below the directory,
/// largest first, with their totals. Chosen types and extensions are listed even when the
/// directory has none, and a query that is an extension the directory lacks is offered too.
pub fn rows(s: &SessionState, query: &str) -> Vec<(Row, Totals)> {
    let q = query.trim().to_ascii_lowercase();
    let size = |t: &Totals| s.size_mode.of(t);
    let breakdown = s.snap.breakdown.as_ref();
    let mut out = Vec::new();
    let all = breakdown
        .map(|b| {
            let mut t = Totals::default();
            for x in &b.by_type {
                t.add(x);
            }
            t
        })
        .unwrap_or_default();
    if q.is_empty() || "all files".contains(&q) {
        out.push((Row::All, all));
    }
    let wanted = |label: &str| q.is_empty() || label.contains(&q);
    let mut types: Vec<(Row, Totals)> = FileType::ALL
        .iter()
        .map(|&t| {
            let tot = breakdown.map(|b| b.by_type[t as usize]);
            (Row::Type(t), tot.unwrap_or_default())
        })
        .filter(|(r, t)| {
            let Row::Type(ty) = r else { return false };
            (t.items > 0 || s.types.has_type(*ty)) && wanted(ty.name())
        })
        .collect();
    types.sort_by_key(|(_, t)| std::cmp::Reverse(size(t)));
    out.extend(types);
    let mut exts: Vec<(Row, Totals)> = breakdown
        .map(|b| b.exts_by(size))
        .unwrap_or_default()
        .into_iter()
        .filter(|(e, _)| wanted(&format!(".{e}")))
        .take(MAX_EXTS)
        .map(|(e, t)| (Row::Ext(e), t))
        .collect();
    for e in s.types.exts() {
        if !exts.iter().any(|(r, _)| *r == Row::Ext(*e)) && wanted(&format!(".{e}")) {
            exts.push((Row::Ext(*e), Totals::default()));
        }
    }
    if let Some(e) = Ext::new(q.as_bytes())
        && !exts.iter().any(|(r, _)| *r == Row::Ext(e))
    {
        exts.push((Row::Ext(e), Totals::default()));
    }
    out.extend(exts);
    out
}

/// Whether `r` is chosen in `f`.
pub fn chosen(f: &TypeFilter, r: Row) -> bool {
    match r {
        Row::All => f.is_empty(),
        Row::Type(t) => f.has_type(t),
        Row::Ext(e) => f.has_ext(&e),
    }
}

/// Chooses `r`, or unchooses it when it is chosen; [`Row::All`] clears the filter. The status
/// line says what is shown now.
pub fn toggle(s: &mut SessionState, r: Row) {
    match r {
        Row::All => s.types = TypeFilter::default(),
        Row::Type(t) => s.types.toggle_type(t),
        Row::Ext(e) => s.types.toggle_ext(e),
    }
    s.status = Some(if s.types.is_empty() {
        "showing all files".into()
    } else {
        format!("showing only {}", s.types)
    });
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    fn rows_of(s: &SessionState, query: &str) -> Vec<Row> {
        rows(s, query).into_iter().map(|(r, _)| r).collect()
    }

    #[test]
    fn choosing_rows_filters_and_says_what_is_shown() {
        let mut s = SessionState::new(Instant::now());
        assert!(!active(&s) && !clear(&mut s));
        toggle(&mut s, Row::Type(FileType::Video));
        assert!(active(&s) && chosen(&s.types, Row::Type(FileType::Video)));
        assert_eq!(s.status.as_deref(), Some("showing only video"));
        assert!(stale(&s, false), "the snapshot counts every file");
        // Without a breakdown, the chosen type is listed all the same, and a query that is an
        // extension is offered.
        assert_eq!(rows_of(&s, ""), [Row::All, Row::Type(FileType::Video)]);
        let mkv = Ext::new(b"mkv").expect("an extension");
        assert!(rows_of(&s, ".mkv").contains(&Row::Ext(mkv)));
        toggle(&mut s, Row::All);
        assert!(!active(&s));
        assert_eq!(s.status.as_deref(), Some("showing all files"));
        toggle(&mut s, Row::Ext(mkv));
        assert!(clear(&mut s) && !active(&s));
    }
}

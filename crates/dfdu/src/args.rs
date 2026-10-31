//! The value parsers of the command-line flags the commands share, and the output format of
//! those that print a table or JSON.

use dfdu_rules::CategoryIx;
use dfdu_tree::Safety;
use dfdu_tree::filetype::TypeFilter;
use dfdu_tui::{Bars, ColorLevel};

/// Why a flag's value was refused: the message clap prints after the flag's name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadValue(String);

impl BadValue {
    pub fn new(message: impl Into<String>) -> BadValue {
        BadValue(message.into())
    }
}

impl std::fmt::Display for BadValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BadValue {}

/// What `report`, `clean`, `diff` and `history` print: a table, or a JSON document.
#[derive(clap::ValueEnum, Clone, Copy, PartialEq)]
pub enum ReportFormat {
    Table,
    Json,
}

/// A size such as `100M`, `1.5GiB` or `500 KB`: `K`, `M`, `G` and `T` alone are binary units.
pub fn size_arg(s: &str) -> Result<u64, BadValue> {
    let t = s.trim();
    let t = match t.strip_suffix(['K', 'M', 'G', 'T']) {
        Some(n) => format!("{n}{}iB", &t[n.len()..]),
        None => t.to_owned(),
    };
    dfdu_rules::parse_size(&t)
        .ok_or_else(|| BadValue(format!("`{s}` is not a size (100M, 1.5GiB, 500KB)")))
}

/// An age in seconds: `30d`, `2w`, `12h`, or a number of days.
pub fn age_arg(s: &str) -> Result<u64, BadValue> {
    let t = s.trim();
    let (num, unit) = t.split_at(t.find(|c: char| !c.is_ascii_digit()).unwrap_or(t.len()));
    let mult = match unit {
        "" | "d" => 86_400,
        "w" => 7 * 86_400,
        "h" => 3_600,
        _ => 0,
    };
    match num.parse::<u64>() {
        Ok(n) if mult > 0 => n
            .checked_mul(mult)
            .ok_or_else(|| BadValue(format!("`{s}` is too long"))),
        _ => Err(BadValue(format!("`{s}` is not an age (30d, 2w, 12h)"))),
    }
}

pub fn safety_arg(s: &str) -> Result<Safety, BadValue> {
    Safety::parse(s).ok_or_else(|| {
        BadValue(format!(
            "`{s}` is not a safety level (safe, regenerable, review, manual, protected)"
        ))
    })
}

/// The categories one `--category` value names: an id, or every id of a family.
#[derive(Clone, Debug)]
pub struct Categories(Vec<CategoryIx>);

impl Categories {
    #[must_use]
    pub fn has(&self, c: CategoryIx) -> bool {
        self.0.contains(&c)
    }
}

pub fn category_arg(s: &str) -> Result<Categories, BadValue> {
    let named = CategoryIx::named(s);
    if named.is_empty() {
        let ids: Vec<&str> = CategoryIx::all().map(|c| c.get().id).collect();
        Err(BadValue(format!(
            "`{s}` is not a category ({})",
            ids.join(", ")
        )))
    } else {
        Ok(Categories(named))
    }
}

/// A non-negative number of seconds, at most a year.
pub fn seconds(s: &str) -> Result<f64, BadValue> {
    match s.parse::<f64>() {
        Ok(v) if (0.0..=31_536_000.0).contains(&v) => Ok(v),
        _ => Err(BadValue(format!("`{s}` is not a number of seconds"))),
    }
}

pub fn type_filter(s: &str) -> Result<TypeFilter, BadValue> {
    let f = TypeFilter::parse(s).map_err(|e| BadValue(e.to_string()))?;
    if f.is_empty() {
        return Err(BadValue::new("name at least one type or extension"));
    }
    Ok(f)
}

/// A `--color` value; `None` detects the level.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColorFlag(pub Option<ColorLevel>);

pub fn color_flag(s: &str) -> Result<ColorFlag, BadValue> {
    dfdu_tui::parse_color_flag(s)
        .map(ColorFlag)
        .map_err(BadValue)
}

pub fn bars_flag(s: &str) -> Result<Bars, BadValue> {
    Bars::parse(s).ok_or_else(|| BadValue(format!("expected {}", Bars::NAMES.join(" or "))))
}

#[cfg(test)]
mod tests;

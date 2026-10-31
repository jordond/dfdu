//! Loading rules: the built-in pack, then the user's rule files in name order. A user rule
//! with the id of an earlier rule replaces it; one that has only `id` and `enabled` turns it on
//! or off. A rule with errors is left out and reported, and the others still load.

use std::collections::HashSet;
use std::path::Path;

use dfdu_tree::Safety;

use crate::schema::{DegradeWhen, File, MatchDef, NativeDef, PerOs, RuleDef};
use crate::vars::Env;
use crate::{
    Action, ActionName, CategoryIx, Degrade, Glob, Guard, Kind, MAX_RULES, Marker, Platform, Rule,
    RuleSet, Source, glob, parse_size,
};

/// The built-in pack.
pub const BUILTIN: &str = include_str!("builtin.toml");

/// A problem with a rule file or one of its rules.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadError {
    /// The file, or `built-in`.
    pub source: String,
    pub rule: Option<String>,
    pub message: String,
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.rule {
            Some(id) => write!(f, "{}: rule `{id}`: {}", self.source, self.message),
            None => write!(f, "{}: {}", self.source, self.message),
        }
    }
}

impl std::error::Error for LoadError {}

/// One rule as loaded, before the set is built: the rule, or a switch for an earlier one.
enum Entry {
    Rule { rule: Box<Rule>, enabled: bool },
    Switch { id: String, enabled: bool },
}

/// The built-in rules for `env`, enabled ones only. Errors here are bugs; tests keep the list
/// empty.
#[must_use]
pub fn builtin(env: &Env) -> (RuleSet, Vec<LoadError>) {
    load_from(env, &[(Source::Builtin, BUILTIN.to_owned())])
}

/// The built-in rules, then every `*.toml` in `user_dir` (in name order) if it exists.
#[must_use]
pub fn load(env: &Env, user_dir: Option<&Path>) -> (RuleSet, Vec<LoadError>) {
    let (files, mut errors) = sources(user_dir);
    let (set, mut more) = load_from(env, &files);
    errors.append(&mut more);
    (set, errors)
}

/// What [`load`] loads: the built-in pack, then the text of every `*.toml` in `user_dir` (in
/// name order) if it exists, and the files that could not be read.
#[must_use]
pub fn sources(user_dir: Option<&Path>) -> (Vec<(Source, String)>, Vec<LoadError>) {
    let mut files = vec![(Source::Builtin, BUILTIN.to_owned())];
    let mut errors = Vec::new();
    if let Some(dir) = user_dir {
        match std::fs::read_dir(dir) {
            Ok(rd) => {
                let mut paths: Vec<_> = rd
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| p.extension().is_some_and(|x| x == "toml"))
                    .collect();
                paths.sort();
                for p in paths {
                    match std::fs::read_to_string(&p) {
                        Ok(text) => files.push((Source::File(p), text)),
                        Err(e) => errors.push(LoadError {
                            source: p.display().to_string(),
                            rule: None,
                            message: e.to_string(),
                        }),
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => errors.push(LoadError {
                source: dir.display().to_string(),
                rule: None,
                message: e.to_string(),
            }),
        }
    }
    (files, errors)
}

/// Loads `files` in order into one set.
#[must_use]
pub fn load_from(env: &Env, files: &[(Source, String)]) -> (RuleSet, Vec<LoadError>) {
    let mut errors = Vec::new();
    // Kept in load order; a later rule with the same id replaces the earlier one in place.
    let mut rules: Vec<(Box<Rule>, bool)> = Vec::new();
    for (source, text) in files {
        for entry in parse_file(env, source, text, &mut errors) {
            match entry {
                Entry::Rule { rule, enabled } => {
                    match rules.iter_mut().find(|(r, _)| r.id == rule.id) {
                        Some(slot) => *slot = (rule, enabled),
                        None => rules.push((rule, enabled)),
                    }
                }
                Entry::Switch { id, enabled } => match rules.iter_mut().find(|(r, _)| r.id == id) {
                    Some(slot) => slot.1 = enabled,
                    None => errors.push(LoadError {
                        source: source_name(source),
                        rule: Some(id),
                        message: "no rule with this id to enable or disable".into(),
                    }),
                },
            }
        }
    }
    let mut enabled: Vec<Rule> = rules
        .into_iter()
        .filter(|(_, on)| *on)
        .map(|(r, _)| *r)
        .collect();
    errors.extend(keep_at_most(&mut enabled, MAX_RULES));
    (RuleSet::new(enabled, env), errors)
}

/// Drops the rules past the first `max`, naming the first one dropped.
fn keep_at_most(rules: &mut Vec<Rule>, max: usize) -> Option<LoadError> {
    let first = rules.get(max)?;
    let error = LoadError {
        source: source_name(&first.source),
        rule: Some(first.id.clone()),
        message: format!(
            "more than {max} rules are enabled; this one and the {} after it are ignored",
            rules.len() - max - 1
        ),
    };
    rules.truncate(max);
    Some(error)
}

fn source_name(s: &Source) -> String {
    match s {
        Source::Builtin => "built-in".into(),
        Source::File(p) => p.display().to_string(),
    }
}

fn parse_file(env: &Env, source: &Source, text: &str, errors: &mut Vec<LoadError>) -> Vec<Entry> {
    let err = |rule: Option<&str>, message: String| LoadError {
        source: source_name(source),
        rule: rule.map(str::to_owned),
        message,
    };
    let file: File = match toml::from_str(text) {
        Ok(f) => f,
        Err(e) => {
            errors.push(err(None, e.to_string().trim_end().to_owned()));
            return Vec::new();
        }
    };
    if file.schema_version != 1 {
        errors.push(err(
            None,
            format!(
                "schema_version {} is not supported (expected 1)",
                file.schema_version
            ),
        ));
        return Vec::new();
    }
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for def in file.rule {
        if !seen.insert(def.id.clone()) {
            errors.push(err(Some(&def.id), "defined twice in this file".into()));
            continue;
        }
        let id = def.id.clone();
        match compile(env, source, def) {
            Ok(Some(e)) => out.push(e),
            Ok(None) => {}
            Err(m) => errors.push(err(Some(&id), m)),
        }
    }
    out
}

fn platforms(list: &[String]) -> Result<Vec<Platform>, String> {
    list.iter()
        .map(|p| Platform::parse(p).ok_or_else(|| format!("unknown platform `{p}`")))
        .collect()
}

/// Expands variables in `patterns` and compiles them. With `names`, patterns must not hold a
/// `/` (they are names, not paths); path patterns also match where a link in them leads
/// ([`Env::link_targets`]).
fn globs(env: &Env, field: &str, patterns: &[String], names: bool) -> Result<Vec<Glob>, String> {
    let mut out = Vec::new();
    for p in patterns {
        if names && p.contains('/') {
            return Err(format!("{field}: `{p}` is a path, not a name"));
        }
        for expanded in env.expand(p).map_err(|e| format!("{field}: {e}"))? {
            let all = if names {
                vec![expanded]
            } else {
                env.link_targets(expanded)
            };
            for one in all {
                let g = Glob::new(&one, glob::platform_folds_case(env.platform))
                    .map_err(|e| format!("{field}: `{p}`: {e}"))?;
                out.push(g);
            }
        }
    }
    Ok(out)
}

/// Compiles one rule. `Ok(None)`: the rule is not for this platform.
fn compile(env: &Env, source: &Source, def: RuleDef) -> Result<Option<Entry>, String> {
    if def.id.is_empty() || def.id.contains(char::is_whitespace) {
        return Err("`id` must be a non-empty dotted name".into());
    }
    let switch_only = def.category.is_none()
        && def.name.is_none()
        && def.safety.is_none()
        && def.matching.is_none();
    if switch_only {
        let Some(enabled) = def.enabled else {
            return Err("a rule needs `category`, `name`, `safety` and `match`".into());
        };
        return Ok(Some(Entry::Switch {
            id: def.id,
            enabled,
        }));
    }
    let need = |v: Option<String>, field: &str| v.ok_or_else(|| format!("`{field}` is missing"));
    let category_id = need(def.category, "category")?;
    let category =
        CategoryIx::of(&category_id).ok_or_else(|| format!("unknown category `{category_id}`"))?;
    let name = need(def.name, "name")?;
    let safety = safety(&need(def.safety, "safety")?)?;
    let action = action(def.action, def.native)?;
    let min_size = match def.min_size {
        Some(s) => parse_size(&s).ok_or_else(|| format!("min_size: cannot read `{s}`"))?,
        None => 0,
    };
    let on = match def.platforms {
        Some(p) => platforms(&p)?,
        None => vec![Platform::MacOs, Platform::Linux, Platform::Windows],
    };
    let verified = def.verified.as_deref().map(platforms).transpose()?;
    if verified.is_some() && *source != Source::Builtin {
        return Err("`verified` is for built-in rules only".into());
    }
    let m = def.matching.ok_or("`match` is missing")?;
    let kind = kind(m.kind.as_deref())?;
    let p = env.platform;
    let (names, paths) = names_and_paths(env, &m, on.contains(&p))?;
    let (larger_than, not_modified_days) = entry_conditions(&m, kind)?;
    let rule = Rule {
        id: def.id,
        category,
        name,
        safety,
        action,
        explanation: def.explanation.unwrap_or_default(),
        lost: def.lost.unwrap_or_default(),
        min_size,
        min_age_days: def.min_age_days.unwrap_or(0),
        priority: def.priority.unwrap_or(50),
        requires_admin: def.requires_admin.unwrap_or(false),
        references: def.references,
        kind,
        names,
        paths,
        exclude: globs(env, "match.exclude", &m.exclude.get(p), false)?,
        not_inside: globs(env, "guard.not_inside", &def.guard.not_inside, false)?,
        stop_descent: m.stop_descent,
        top_most: m.top_most,
        sibling: m.sibling.into_iter().map(|a| a.any_of).collect(),
        child: m.child.into_iter().map(|a| a.any_of).collect(),
        markers: m
            .marker
            .into_iter()
            .map(|mk| Marker {
                file: mk.file,
                prefix: mk.prefix.map(String::into_bytes),
                required: mk.required,
            })
            .collect(),
        larger_than,
        not_modified_days,
        empty: m.empty,
        broken_link: m.broken_link,
        guard: Guard {
            not_running: def.guard.not_running.get(p),
            not_vcs_tracked: def.guard.not_vcs_tracked,
        },
        degrade: degrade(def.degrade.when)?,
        source: source.clone(),
    };
    if rule.degrade.iter().any(|d| d.safety < rule.safety) {
        return Err("degrade: a condition may only lower safety, not raise it".into());
    }
    if !on.contains(&p) {
        return Ok(None);
    }
    let enabled = def.enabled.unwrap_or(true) && verified.is_none_or(|v| v.contains(&p));
    Ok(Some(Entry::Rule {
        rule: Box::new(rule),
        enabled,
    }))
}

/// The `safety` of a rule; [`Safety::Protected`] is not one a rule can give.
fn safety(s: &str) -> Result<Safety, String> {
    Safety::parse(s)
        .filter(|&s| s != Safety::Protected)
        .ok_or_else(|| format!("unknown safety `{s}` (safe, regenerable, review or manual)"))
}

/// The `action` of a rule, `delete` when not set. `native` needs a `native.command`, and
/// `[rule.native]` is only for `native`.
fn action(s: Option<String>, native: Option<NativeDef>) -> Result<Action, String> {
    let name = match s.as_deref() {
        None => ActionName::Delete,
        Some(s) => ActionName::parse(s)
            .ok_or_else(|| format!("unknown action `{s}` (delete, trash, native or report)"))?,
    };
    match (name, native) {
        (ActionName::Native, Some(n)) if !n.command.is_empty() => Ok(Action::Native(Box::new(n))),
        (ActionName::Native, _) => Err("`action = \"native\"` needs `native.command`".into()),
        (name, Some(_)) => Err(format!(
            "`[rule.native]` is for `action = \"native\"`, not `{}`",
            name.as_str()
        )),
        (ActionName::Delete, None) => Ok(Action::Delete),
        (ActionName::Trash, None) => Ok(Action::Trash),
        (ActionName::Report, None) => Ok(Action::Report),
    }
}

/// `match.kind`, `any` when not set.
fn kind(k: Option<&str>) -> Result<Kind, String> {
    let k = k.unwrap_or("any");
    Kind::parse(k).ok_or_else(|| format!("match.kind: unknown kind `{k}` (dir, file or any)"))
}

/// `match.names` and `match.paths` for this platform. A rule for it (`for_here`) needs one of
/// them unless they are given per platform.
fn names_and_paths(
    env: &Env,
    m: &MatchDef,
    for_here: bool,
) -> Result<(Vec<Glob>, Vec<Glob>), String> {
    let names_src = m.names.get(env.platform);
    let paths_src = m.paths.get(env.platform);
    let names = globs(env, "match.names", &names_src, true)?;
    let paths = globs(env, "match.paths", &paths_src, false)?;
    // A rule for another platform may have no patterns on this one.
    if names_src.is_empty()
        && paths_src.is_empty()
        && for_here
        && !per_os(&m.names)
        && !per_os(&m.paths)
    {
        return Err("match needs `names` or `paths`".into());
    }
    Ok((names, paths))
}

/// The `larger_than` and `not_modified_days` of `m` (0 when not set), after checking that
/// its conditions on the entry itself suit `kind`.
fn entry_conditions(m: &MatchDef, kind: Kind) -> Result<(u64, u32), String> {
    let larger_than = match &m.larger_than {
        Some(s) => parse_size(s).ok_or_else(|| format!("match.larger_than: cannot read `{s}`"))?,
        None => 0,
    };
    let not_modified_days = m.not_modified_days.unwrap_or(0);
    if (larger_than > 0 || not_modified_days > 0) && kind != Kind::File {
        return Err("match.larger_than and match.not_modified_days need `kind = \"file\"`".into());
    }
    if m.broken_link && kind != Kind::File {
        return Err("match.broken_link needs `kind = \"file\"`".into());
    }
    if m.empty && kind != Kind::Dir {
        return Err("match.empty needs `kind = \"dir\"`".into());
    }
    Ok((larger_than, not_modified_days))
}

/// The `degrade.when` conditions with their safety parsed.
fn degrade(when: Vec<DegradeWhen>) -> Result<Vec<Degrade>, String> {
    when.into_iter()
        .map(|w| {
            let safety = Safety::parse(&w.safety)
                .ok_or_else(|| format!("degrade: unknown safety `{}`", w.safety))?;
            Ok(Degrade {
                missing_sibling: w.missing_sibling,
                safety,
                reason: w.reason,
            })
        })
        .collect()
}

fn per_os<T>(v: &PerOs<T>) -> bool {
    matches!(v, PerOs::Each { .. })
}

#[cfg(test)]
mod tests;

//! Loading rules: the built-in pack, then the user's rule files in name order. A user rule
//! with the id of an earlier rule replaces it; one that has only `id` and `enabled` turns it on
//! or off. A rule with errors is left out and reported, and the others still load.

use std::collections::HashSet;
use std::path::Path;

use crate::schema::{File, PerOs, RuleDef};
use crate::vars::Env;
use crate::{
    Action, Degrade, Glob, Guard, Kind, Marker, Platform, Rule, RuleSet, Safety, Source,
    category_index, glob, parse_size,
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

/// One rule as loaded, before the set is built: the rule, or a switch for an earlier one.
enum Entry {
    Rule { rule: Box<Rule>, enabled: bool },
    Switch { id: String, enabled: bool },
}

/// The built-in rules for `env`, enabled ones only. Errors here are bugs; tests keep the list
/// empty.
pub fn builtin(env: &Env) -> (RuleSet, Vec<LoadError>) {
    load_from(env, &[(Source::Builtin, BUILTIN.to_owned())])
}

/// The built-in rules, then every `*.toml` in `user_dir` (in name order) if it exists.
pub fn load(env: &Env, user_dir: Option<&Path>) -> (RuleSet, Vec<LoadError>) {
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
    let (set, mut more) = load_from(env, &files);
    errors.append(&mut more);
    (set, errors)
}

/// Loads `files` in order into one set.
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
    let enabled = rules
        .into_iter()
        .filter(|(_, on)| *on)
        .map(|(r, _)| *r)
        .collect();
    (RuleSet::new(enabled, env), errors)
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

/// Expands variables in `patterns` and compiles them. Patterns of `kind` `names` must not hold
/// a `/`.
fn globs(env: &Env, field: &str, patterns: &[String], names: bool) -> Result<Vec<Glob>, String> {
    let mut out = Vec::new();
    for p in patterns {
        if names && p.contains('/') {
            return Err(format!("{field}: `{p}` is a path, not a name"));
        }
        for expanded in env.expand(p).map_err(|e| format!("{field}: {e}"))? {
            let g = Glob::new(&expanded, glob::platform_folds_case(env.platform))
                .map_err(|e| format!("{field}: `{p}`: {e}"))?;
            out.push(g);
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
        category_index(&category_id).ok_or_else(|| format!("unknown category `{category_id}`"))?;
    let name = need(def.name, "name")?;
    let safety_s = need(def.safety, "safety")?;
    let safety = Safety::parse(&safety_s)
        .filter(|&s| s != Safety::Protected)
        .ok_or_else(|| {
            format!("unknown safety `{safety_s}` (safe, regenerable, review or manual)")
        })?;
    let action_s = def.action.unwrap_or_else(|| "delete".into());
    let action = Action::parse(&action_s)
        .ok_or_else(|| format!("unknown action `{action_s}` (delete, trash, native or report)"))?;
    if action == Action::Native && def.native.as_ref().is_none_or(|n| n.command.is_empty()) {
        return Err("`action = \"native\"` needs `native.command`".into());
    }
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
    let kind = match m.kind.as_deref().unwrap_or("any") {
        "dir" => Kind::Dir,
        "file" => Kind::File,
        "any" => Kind::Any,
        k => return Err(format!("match.kind: unknown kind `{k}` (dir, file or any)")),
    };
    let p = env.platform;
    let names_src = m.names.get(p);
    let paths_src = m.paths.get(p);
    let names = globs(env, "match.names", &names_src, true)?;
    let paths = globs(env, "match.paths", &paths_src, false)?;
    if names_src.is_empty() && paths_src.is_empty() {
        // A rule for another platform may have no patterns on this one.
        if on.contains(&p) && !per_os(&m.names) && !per_os(&m.paths) {
            return Err("match needs `names` or `paths`".into());
        }
    }
    let larger_than = match &m.larger_than {
        Some(s) => parse_size(s).ok_or_else(|| format!("match.larger_than: cannot read `{s}`"))?,
        None => 0,
    };
    let not_modified_days = m.not_modified_days.unwrap_or(0);
    if (larger_than > 0 || not_modified_days > 0) && kind != Kind::File {
        return Err("match.larger_than and match.not_modified_days need `kind = \"file\"`".into());
    }
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
        guard: Guard {
            not_running: def.guard.not_running.get(p),
            not_vcs_tracked: def.guard.not_vcs_tracked,
            keep_newest: def.guard.keep_newest,
            group_by: def.guard.group_by,
        },
        degrade: def
            .degrade
            .when
            .into_iter()
            .map(|w| {
                let safety = Safety::parse(&w.safety)
                    .ok_or_else(|| format!("degrade: unknown safety `{}`", w.safety))?;
                Ok(Degrade {
                    missing_sibling: w.missing_sibling,
                    safety,
                    reason: w.reason,
                })
            })
            .collect::<Result<_, String>>()?,
        native: def.native,
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

fn per_os<T>(v: &PerOs<T>) -> bool {
    matches!(v, PerOs::Each { .. })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> Env {
        Env::with(Platform::Linux, &[("HOME", "/home/u")])
    }

    fn user(text: &str) -> (RuleSet, Vec<LoadError>) {
        load_from(
            &env(),
            &[
                (Source::Builtin, BUILTIN.to_owned()),
                (Source::File("/rules/mine.toml".into()), text.to_owned()),
            ],
        )
    }

    #[test]
    fn the_builtin_pack_loads_without_errors_on_every_platform() {
        for p in [Platform::MacOs, Platform::Linux, Platform::Windows] {
            let e = Env::with(
                p,
                &[
                    ("HOME", "/h"),
                    ("USERPROFILE", "C:/Users/u"),
                    ("LOCALAPPDATA", "C:/Users/u/AppData/Local"),
                    ("APPDATA", "C:/Users/u/AppData/Roaming"),
                    ("TEMP", "C:/Users/u/AppData/Local/Temp"),
                ],
            );
            let (set, errors) = builtin(&e);
            assert!(errors.is_empty(), "{p:?}: {errors:#?}");
            assert!(!set.rules().is_empty(), "{p:?}");
        }
    }

    #[test]
    fn errors_name_the_file_and_rule() {
        let (_, errors) = user(
            r#"
schema_version = 1
[[rule]]
id = "x.bad"
category = "dev.nope"
name = "Bad"
safety = "safe"
[rule.match]
names = ["x"]
"#,
        );
        assert_eq!(
            errors.iter().map(|e| e.to_string()).collect::<Vec<_>>(),
            ["/rules/mine.toml: rule `x.bad`: unknown category `dev.nope`"]
        );
    }

    #[test]
    fn misspelt_keys_and_bad_values_are_reported() {
        for (body, want) in [
            ("[rule.match]\nnamez = [\"x\"]", "unknown field `namez`"),
            ("safety2 = 1", "unknown field `safety2`"),
            (
                "min_size = \"10 parsecs\"\n[rule.match]\nnames = [\"x\"]",
                "min_size: cannot read",
            ),
            ("[rule.match]\nnames = [\"a/b\"]", "is a path, not a name"),
            (
                "[rule.match]\nkind = \"pipe\"\nnames = [\"x\"]",
                "unknown kind `pipe`",
            ),
            (
                "[rule.match]\npaths = [\"{nope}/x\"]",
                "unknown variable `{nope}`",
            ),
            (
                "[rule.match]\nnames = [\"x**\"]",
                "`**` must be a whole path component",
            ),
            (
                "action = \"native\"\n[rule.match]\nnames = [\"x\"]",
                "needs `native.command`",
            ),
            ("[rule.match]\nkind = \"dir\"", "needs `names` or `paths`"),
            (
                "[rule.match]\nnames = [\"x\"]\nnot_modified_days = 9",
                "need `kind = \"file\"`",
            ),
            (
                "[rule.match]\nkind = \"file\"\nnames = [\"x\"]\nlarger_than = \"big\"",
                "match.larger_than: cannot read",
            ),
            (
                "[rule.match]\nnames = [\"x\"]\n[[rule.degrade.when]]\nsafety = \"safe\"\nreason = \"r\"",
                "may only lower safety",
            ),
        ] {
            let text = format!(
                "schema_version = 1\n[[rule]]\nid = \"t.r\"\ncategory = \"sys.cache\"\nname = \"T\"\nsafety = \"regenerable\"\n{body}\n"
            );
            let (_, errors) = user(&text);
            assert!(
                errors.len() == 1 && errors[0].to_string().contains(want),
                "{body:?}: {errors:#?}"
            );
        }
        let (_, errors) = user("schema_version = 2");
        assert!(errors[0].message.contains("schema_version 2"));
        let (_, errors) = user("schema_version = 1\n[[rule]]\nid = \"t\"");
        assert!(errors[0].message.contains("needs `category`"));
    }

    #[test]
    fn user_rules_replace_disable_and_add() {
        let base = builtin(&env()).0;
        assert!(base.get("node.node_modules").is_some());
        let (set, errors) = user(
            r#"
schema_version = 1
[[rule]]
id = "node.node_modules"
enabled = false

[[rule]]
id = "rust.target"
category = "dev.build"
name = "My target dirs"
safety = "review"
[rule.match]
kind = "dir"
names = ["target"]

[[rule]]
id = "me.renders"
category = "content.media"
name = "Render output"
safety = "regenerable"
[rule.match]
paths = ["{home}/renders/*"]
"#,
        );
        assert!(errors.is_empty(), "{errors:#?}");
        assert!(set.get("node.node_modules").is_none());
        let t = set.get("rust.target").expect("replaced");
        assert_eq!(
            (t.name.as_str(), t.safety),
            ("My target dirs", Safety::Review)
        );
        assert!(matches!(t.source, Source::File(_)));
        assert_eq!(set.rules().len(), base.rules().len());
        let r = set.get("me.renders").expect("added");
        assert!(r.paths[0].matches(b"/home/u/renders/x"));
    }

    #[test]
    fn a_bad_rule_does_not_stop_the_others() {
        let (set, errors) = user(
            "schema_version = 1\n[[rule]]\nid = \"a\"\ncategory = \"sys.cache\"\nname = \"A\"\nsafety = \"bogus\"\n[rule.match]\nnames = [\"a\"]\n\n[[rule]]\nid = \"b\"\ncategory = \"sys.cache\"\nname = \"B\"\nsafety = \"safe\"\n[rule.match]\nnames = [\"b\"]\n",
        );
        assert_eq!(errors.len(), 1);
        assert!(set.get("b").is_some());
        let (_, errors) = user("schema_version = 1\n[[rule]]\nid = \"nope\"\nenabled = true\n");
        assert!(errors[0].message.contains("no rule with this id"));
    }

    #[test]
    fn rules_for_other_platforms_and_unverified_rules_stay_off() {
        let text = r#"
schema_version = 1
[[rule]]
id = "mac.only"
category = "sys.cache"
name = "M"
safety = "safe"
platforms = ["macos"]
[rule.match]
names = ["m"]

[[rule]]
id = "checked.on.mac"
category = "sys.cache"
name = "C"
safety = "safe"
verified = ["macos"]
[rule.match]
names = ["c"]
"#;
        let files = [(Source::Builtin, text.to_owned())];
        let (set, errors) = load_from(&env(), &files);
        assert!(errors.is_empty(), "{errors:#?}");
        assert!(set.get("mac.only").is_none() && set.get("checked.on.mac").is_none());
        let mac = Env::with(Platform::MacOs, &[("HOME", "/Users/u")]);
        let (set, _) = load_from(&mac, &files);
        assert!(set.get("mac.only").is_some() && set.get("checked.on.mac").is_some());
        // A user can turn on a rule that was not verified on their platform.
        let mut files = files.to_vec();
        files.push((
            Source::File("/r.toml".into()),
            "schema_version = 1\n[[rule]]\nid = \"checked.on.mac\"\nenabled = true\n".into(),
        ));
        let (set, errors) = load_from(&env(), &files);
        assert!(errors.is_empty(), "{errors:#?}");
        assert!(set.get("checked.on.mac").is_some());
        // `verified` is not for user files.
        let (_, errors) = user(&text.replace("mac.only", "x").replace("checked.on.mac", "y"));
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("built-in rules only"))
        );
    }

    #[test]
    fn missing_user_dir_is_fine() {
        let (set, errors) = load(&env(), Some(Path::new("/nonexistent/dfdu/rules")));
        assert!(errors.is_empty());
        assert!(!set.rules().is_empty());
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("b.toml"),
            "schema_version = 1\n[[rule]]\nid = \"node.node_modules\"\nenabled = false\n",
        )
        .expect("write");
        std::fs::write(dir.path().join("a.toml"), "not toml [").expect("write");
        std::fs::write(dir.path().join("notes.txt"), "ignored").expect("write");
        let (set, errors) = load(&env(), Some(dir.path()));
        assert_eq!(errors.len(), 1, "{errors:#?}");
        assert!(errors[0].source.ends_with("a.toml"));
        assert!(set.get("node.node_modules").is_none());
    }
}

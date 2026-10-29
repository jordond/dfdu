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
fn rules_past_the_limit_are_dropped_and_reported() {
    let (set, errors) = user(
        r#"
schema_version = 1
[[rule]]
id = "x.a"
category = "dev.build"
name = "A"
safety = "safe"
[rule.match]
names = ["a"]
"#,
    );
    assert!(errors.is_empty(), "{errors:#?}");
    let mut rules = set.rules().to_vec();
    let n = rules.len();
    assert!(keep_at_most(&mut rules, n).is_none());
    let error = keep_at_most(&mut rules, n - 1).expect("over the limit");
    assert_eq!(rules.len(), n - 1);
    assert_eq!(
        error.to_string(),
        format!(
            "/rules/mine.toml: rule `x.a`: more than {} rules are enabled; this one and the \
             0 after it are ignored",
            n - 1
        )
    );
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
        errors
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>(),
        ["/rules/mine.toml: rule `x.bad`: unknown category `dev.nope`"]
    );
}

#[test]
fn misspelt_keys_and_bad_values_are_reported() {
    for (body, want) in [
        ("[rule.match]\nnamez = [\"x\"]", "unknown field `namez`"),
        ("safety2 = 1", "unknown field `safety2`"),
        // An old guard that was never enforced: refused rather than ignored.
        (
            "[rule.match]\nnames = [\"x\"]\n[rule.guard]\nkeep_newest = 1",
            "unknown field `keep_newest`",
        ),
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
        (
            "action = \"trash\"\n[rule.match]\nnames = [\"x\"]\n[rule.native]\ncommand = [\"x\"]",
            "`[rule.native]` is for `action = \"native\"`, not `trash`",
        ),
        ("[rule.match]\nkind = \"dir\"", "needs `names` or `paths`"),
        (
            "[rule.match]\nnames = [\"x\"]\nnot_modified_days = 9",
            "need `kind = \"file\"`",
        ),
        (
            "[rule.match]\nnames = [\"x\"]\nempty = true",
            "match.empty needs `kind = \"dir\"`",
        ),
        (
            "[rule.match]\nkind = \"dir\"\nnames = [\"x\"]\nbroken_link = true",
            "match.broken_link needs `kind = \"file\"`",
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
    assert!(errors.is_empty(), "{errors:#?}");
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

/// `~/.gradle` and `~/.cargo` are links to another disk, which the scan of the home folder
/// does not follow: the rules and protected paths there also apply where they lead.
#[cfg(unix)]
#[test]
fn path_patterns_follow_links_to_where_they_lead() {
    use crate::{Candidate, Verdict};
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(tmp.path()).expect("canonical");
    let (home, data) = (root.join("home"), root.join("data"));
    for d in ["home", "data/gradle/caches", "data/cargo/bin"] {
        std::fs::create_dir_all(root.join(d)).expect("mkdir");
    }
    std::os::unix::fs::symlink(data.join("gradle"), home.join(".gradle")).expect("link");
    std::os::unix::fs::symlink(data.join("cargo"), home.join(".cargo")).expect("link");
    let home_s = home.to_str().expect("utf-8");
    let data_s = data.to_str().expect("utf-8").to_owned();
    // On Linux, whose protected paths leave the temporary folder alone; the Gradle rule
    // turned on there.
    let on = "schema_version = 1\n[[rule]]\nid = \"gradle.user-caches\"\nenabled = true\n";
    let files = [
        (Source::Builtin, BUILTIN.to_owned()),
        (Source::File("/rules/on.toml".into()), on.to_owned()),
    ];
    let classify = |env: &Env, parent: &str, name: &str| {
        let (set, errors) = load_from(env, &files);
        assert!(errors.is_empty(), "{errors:#?}");
        let no = |_: &[u8]| false;
        let c = Candidate {
            parent: parent.as_bytes(),
            name: name.as_bytes(),
            is_dir: true,
            sibling: &no,
            inside: &[],
            protection: set.protected().state_of(parent.as_bytes()),
            partial: false,
            stat: None,
            link: None,
        };
        match set.classify(&c) {
            // Without the CACHEDIR.TAG rule, which every directory is a candidate for.
            Verdict::Hits(h) => (h.iter().map(|h| set.rule(h.rule).id.clone()))
                .filter(|id| id != "generic.cachedir-tag")
                .collect(),
            Verdict::Protected(_) => vec!["protected".into()],
            Verdict::None => vec![],
        }
    };
    let env = Env::with(Platform::Linux, &[("HOME", home_s)]);
    let gradle = format!("{data_s}/gradle");
    let cargo = format!("{data_s}/cargo");
    assert_eq!(classify(&env, &gradle, "caches"), Vec::<String>::new());
    let env = env.resolving_links();
    assert_eq!(classify(&env, &gradle, "caches"), ["gradle.user-caches"]);
    assert_eq!(classify(&env, &cargo, "bin"), ["protected"]);
    // Where the link is, the rule still applies (a scan of the link itself).
    let at_link = format!("{home_s}/.gradle");
    assert_eq!(classify(&env, &at_link, "caches"), ["gradle.user-caches"]);
}

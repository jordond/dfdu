//! What `scan` and `report` add about what the scan did not enter: mounts it listed but stayed
//! out of, with their type and used space ([`dfdu_engine::not_entered`]), and rule locations
//! behind a link that leads out of the scan ([`dfdu_rules::linked_outside`]). Printed as footer lines,
//! and as the optional `not_entered` and `links_outside` fields of the JSON documents.

use std::io::{self, Write};

use dfdu_engine::{LIST_MAX, Model, NotEntered};
use dfdu_rules::{Linked, RuleSet};
use dfdu_sys::MountInfo;
use dfdu_tree::Ix;
use serde_json::{Map, Value};

use super::{put_host_path, put_path};

/// What was looked up once the scan ended.
pub struct Outside {
    pub mounts: Vec<(NotEntered, Option<MountInfo>)>,
    pub linked: Vec<Linked>,
    /// The scan is of this machine: its paths below the home directory are shown with `~`.
    pub local: bool,
}

/// The mounts below `root` the scan did not enter and the links out of it. With `local` unset
/// (a scan on another host) nothing is looked up on this machine: the mounts are listed by
/// path alone and links are not looked for.
pub fn gather(m: &Model, root: Ix, local: bool) -> Outside {
    let list = dfdu_engine::not_entered(&m.tree, root);
    let infos = if local && !list.is_empty() {
        let paths: Vec<_> = list.iter().map(|n| n.path.to_local()).collect();
        dfdu_sys::mounts_at(&paths, dfdu_sys::DISK_TIMEOUT)
    } else {
        vec![None; list.len()]
    };
    let linked = match &m.rules {
        Some(r) if local => dfdu_rules::linked_outside(r, &m.tree.path_buf(root)),
        _ => Vec::new(),
    };
    Outside {
        mounts: list.into_iter().zip(infos).collect(),
        linked,
        local,
    }
}

/// The footer lines: the mounts not entered, then the links, at most [`LIST_MAX`] of each
/// named. The link notes suggest `command` (`dfdu report`) on where they lead.
pub fn lines(o: &Outside, rules: Option<&RuleSet>, command: &str) -> Vec<String> {
    let home = dfdu_engine::home_dir().filter(|_| o.local);
    let mut out = dfdu_engine::not_entered_lines(&o.mounts, home.as_deref());
    let Some(rules) = rules else {
        return out;
    };
    for l in o.linked.iter().take(LIST_MAX) {
        out.push(dfdu_engine::linked_note(rules, l, home.as_deref(), command));
    }
    out.extend(dfdu_engine::more_links(o.linked.len(), LIST_MAX));
    out
}

/// Prints `dfdu scan`'s footer lines about what the scan below `root` did not enter to stderr
/// ([`gather`] with `local`).
pub fn footer(m: &Model, root: Ix, local: bool) {
    let left_out = gather(m, root, local);
    let lines = lines(&left_out, m.rules.as_deref(), "dfdu scan");
    let _ = print(&mut io::stderr().lock(), &lines);
}

/// Writes `lines` to `w`, one per line.
pub fn print(w: &mut dyn Write, lines: &[String]) -> io::Result<()> {
    for l in lines {
        writeln!(w, "{l}")?;
    }
    w.flush()
}

/// Adds `not_entered` and `links_outside` to `doc`, each only when it has entries.
pub fn put_json(doc: &mut Map<String, Value>, o: &Outside, rules: Option<&RuleSet>) {
    if !o.mounts.is_empty() {
        let list: Vec<Value> = o
            .mounts
            .iter()
            .map(|(n, info)| {
                let mut e = Map::new();
                put_host_path(&mut e, "path", &n.path);
                e.insert("network".into(), n.network.into());
                e.insert("option".into(), n.option().into());
                if let Some(i) = info {
                    e.insert("fstype".into(), i.fstype.clone().into());
                    if let Some(u) = i.used {
                        e.insert("used".into(), u.into());
                    }
                }
                Value::Object(e)
            })
            .collect();
        doc.insert("not_entered".into(), list.into());
    }
    if let Some(rules) = rules.filter(|_| !o.linked.is_empty()) {
        let list: Vec<Value> = o
            .linked
            .iter()
            .map(|l| {
                let mut e = Map::new();
                put_path(&mut e, "link", &l.link);
                put_path(&mut e, "target", &l.target);
                let ids: Vec<Value> = (l.rules.iter())
                    .map(|&r| rules.rule(r).id.clone().into())
                    .collect();
                e.insert("rules".into(), ids.into());
                Value::Object(e)
            })
            .collect();
        doc.insert("links_outside".into(), list.into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_fields_are_left_out_when_empty() {
        let o = Outside {
            mounts: vec![],
            linked: vec![],
            local: true,
        };
        let mut doc = Map::new();
        put_json(&mut doc, &o, None);
        assert!(doc.is_empty());
        let o = Outside {
            mounts: vec![(
                NotEntered {
                    path: "/mnt/data".into(),
                    network: false,
                },
                Some(MountInfo {
                    fstype: "zfs".into(),
                    class: dfdu_sys::FsClass::Local,
                    used: Some(7),
                    total: Some(70),
                }),
            )],
            linked: vec![],
            local: true,
        };
        put_json(&mut doc, &o, None);
        assert_eq!(
            Value::Object(doc),
            serde_json::json!({ "not_entered": [
                { "path": "/mnt/data", "network": false, "option": "--cross-mounts",
                  "fstype": "zfs", "used": 7 }
            ]})
        );
    }
}

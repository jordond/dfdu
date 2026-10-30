//! Classes and protection against a reference classifier, and the classifier pool's answers.

use std::collections::HashSet;

use dfdu_engine::{CheckMsg, CheckReq, Model};
use dfdu_rules::protected::State;
use dfdu_rules::{Candidate, Inside, RuleIx, RuleSet, Verdict};
use dfdu_tree::{Class, DirState, Ix, Kind};

use crate::{ROOT, ROOT_NAME, Spec};

/// A directory [`check_classes`] has still to walk, with the rule state carried down to it.
struct Frame {
    /// Its node in the spec.
    spec_node: u32,
    /// Its node in the tree.
    ix: Ix,
    path: Vec<u8>,
    /// The rules decided above it.
    inside: Vec<RuleIx>,
    /// Its protection.
    prot: State,
    /// The class in effect at it.
    above: Option<Class>,
}

/// Every node's class in effect and protection against a reference classifier run on the spec,
/// over every directory whose listing and whose parents' listings completed (a directory left
/// partial was classified on what was read). The class in effect is the nearest match, or the
/// highest one above whose rule stops the descent ([`RuleSet::class_in_effect`]): comparing it
/// leaves out matches below such a rule's, which the scan makes only when that match was
/// decided after its listing's subdirectories were queued (`dfdu_scan::classify`).
/// Returns the number of classified nodes.
#[expect(
    clippy::too_many_lines,
    reason = "one walk of the tree carrying each node's rule state down"
)]
pub(crate) fn check_classes(spec: &Spec, model: &Model, rules: &RuleSet) -> Result<u64, String> {
    let tree = &model.tree;
    let Some(root) = tree.roots().next() else {
        return Ok(0);
    };
    let complete = |ix: Ix| tree.dir(ix).is_some_and(|d| d.state == DirState::Complete);
    let mut stack = vec![Frame {
        spec_node: ROOT,
        ix: root,
        path: ROOT_NAME.as_bytes().to_vec(),
        inside: Vec::new(),
        prot: State::Off,
        above: None,
    }];
    let mut classified = 0;
    while let Some(Frame {
        spec_node: sn,
        ix,
        path,
        inside,
        prot,
        above,
    }) = stack.pop()
    {
        if !complete(ix) {
            continue;
        }
        let kids: Vec<u32> = spec.live_children(sn).collect();
        let names: HashSet<&[u8]> = kids
            .iter()
            .map(|&c| &spec.nodes[c as usize].name[..])
            .collect();
        let sibling = |n: &[u8]| names.contains(n);
        let dir = rules.dir(&path);
        for &c in &kids {
            let cn = &spec.nodes[c as usize];
            let is_dir = cn.meta.kind == Kind::Dir;
            let cix = tree
                .children(ix)
                .find(|&t| tree.name(t) == &cn.name[..])
                .ok_or_else(|| format!("missing {}", String::from_utf8_lossy(&cn.name)))?;
            let cpath = [&path[..], b"/", &cn.name].concat();
            let show = || String::from_utf8_lossy(&cpath).into_owned();
            let verdict = dir.classify(&Candidate {
                parent: &path,
                name: &cn.name,
                is_dir,
                sibling: &sibling,
                inside: &inside,
                protection: prot,
                partial: false,
                stat: (!is_dir).then_some(dfdu_rules::Stat {
                    disk: cn.meta.alloc,
                    mtime: cn.meta.mtime,
                }),
                link: None,
            });
            let (mut cprot, mut own) = (State::Off, None);
            match verdict {
                Verdict::Protected(st) => cprot = st,
                Verdict::None => {}
                Verdict::Hits(hits) => {
                    // Its own listing as the scan saw it: vanished entries are listed but
                    // not stat'ed, so they are there but are not files to read.
                    let inner: Vec<u32> = if is_dir {
                        spec.live_children(c).collect()
                    } else {
                        vec![]
                    };
                    let child = |n: &[u8]| inner.iter().any(|&k| spec.nodes[k as usize].name == n);
                    let marker = |f: &[u8], len: usize| {
                        let k = inner.iter().find(|&&k| spec.nodes[k as usize].name == f)?;
                        let n = &spec.nodes[*k as usize];
                        (n.meta.kind == Kind::File && !n.vanished)
                            .then(|| n.content[..len.min(n.content.len())].to_vec())
                    };
                    // Listings below that are not complete were looked into on what was read.
                    let looked = !is_dir || complete(cix);
                    own = hits.iter().find_map(|h| {
                        let ok = match h.inside {
                            Inside::Done => true,
                            Inside::Check => {
                                is_dir
                                    && rules.check_inside(
                                        h.rule,
                                        &cn.name,
                                        &child,
                                        &marker,
                                        inner.is_empty(),
                                    )
                            }
                        };
                        ok.then_some(Class {
                            rule: h.rule,
                            safety: h.safety,
                            degraded: h.degraded,
                            flags: 0,
                        })
                    });
                    if !looked {
                        continue;
                    }
                }
            }
            // The nearest match, unless the one in effect above stops the descent.
            let mut want = match above {
                Some(a) if rules.stops_descent(a.rule) => above,
                _ => own.or(above),
            };
            // The classifier pool's flags, and the protection a git-tracked match gets, are not
            // the scan's to decide (`check_pool_flags`).
            let in_effect = rules.class_in_effect(tree, cix);
            let got = in_effect.map(|(_, c)| Class { flags: 0, ..c });
            if let (Some(w), Some((_, c))) = (want.as_mut(), in_effect)
                && c.has(Class::VCS_TRACKED)
            {
                w.safety = c.safety;
            }
            if got != want {
                return Err(format!("{}: class {got:?}, expected {want:?}", show()));
            }
            if tree.is_protected(cix) != cprot.protected() {
                return Err(format!("{}: protected {}", show(), !cprot.protected()));
            }
            classified += u64::from(own.is_some());
            if is_dir {
                let mut ci = inside.clone();
                ci.extend(own.map(|c| c.rule));
                stack.push(Frame {
                    spec_node: c,
                    ix: cix,
                    path: cpath,
                    inside: ci,
                    prot: cprot,
                    above: want,
                });
            }
        }
    }
    Ok(classified)
}

/// The classifier pool's answer in the simulation, decided by a hash of the node's path: git
/// tracks a third of guarded matches, cannot tell for another third, and a process a rule names
/// runs for a quarter. An answer applied to another node than it was asked for shows up as
/// flags that do not match that node's path.
#[must_use]
pub fn sim_check(req: &CheckReq) -> CheckMsg {
    match req {
        CheckReq::Guards {
            node,
            rule,
            path,
            vcs,
            processes,
        } => {
            let (tracked, running) = pool_answer(path.as_bytes(), *vcs, processes);
            CheckMsg::Guards {
                node: *node,
                rule: *rule,
                tracked,
                running,
            }
        }
        CheckReq::NativeSize { rule, .. } => CheckMsg::NativeSize {
            rule: *rule,
            result: Ok(0),
        },
    }
}

fn pool_answer(path: &[u8], vcs: bool, processes: &[String]) -> (Option<bool>, Option<String>) {
    let h = path.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
    });
    let tracked = match (vcs, h % 3) {
        (false, _) | (true, 1) => Some(false),
        (true, 0) => Some(true),
        _ => None,
    };
    let running = (h / 3 % 4 == 0)
        .then(|| processes.first().cloned())
        .flatten();
    (tracked, running)
}

/// Every visible class with a guard was checked, has the flags [`sim_check`] gives its path,
/// and is protected when git tracks it. Returns the classes flagged tracked and in use.
pub(crate) fn check_pool_flags(model: &Model, rules: &RuleSet) -> Result<(u64, u64), String> {
    let tree = &model.tree;
    let (mut tracked, mut in_use) = (0, 0);
    for (ix, c) in tree.classes().filter(|(ix, _)| tree.is_visible(*ix)) {
        let g = &rules.rule(c.rule).guard;
        if !g.not_vcs_tracked && g.not_running.is_empty() {
            continue;
        }
        // Hashed as the request had it.
        let path = tree.path_buf(ix);
        let show = || format!("{} ({c:?})", path.display());
        if !c.has(Class::CHECKED) || c.has(Class::QUEUED) {
            return Err(format!("{}: not checked", show()));
        }
        let (t, r) = pool_answer(
            path.as_os_str().as_encoded_bytes(),
            g.not_vcs_tracked,
            &g.not_running,
        );
        let t = t == Some(true);
        if c.has(Class::VCS_TRACKED) != t || c.has(Class::IN_USE) != r.is_some() {
            return Err(format!(
                "{}: flags do not match the answer for its path",
                show()
            ));
        }
        if t && c.safety != dfdu_rules::Safety::Protected {
            return Err(format!("{}: git-tracked but not protected", show()));
        }
        tracked += u64::from(t);
        in_use += u64::from(r.is_some());
    }
    Ok((tracked, in_use))
}

#[cfg(test)]
mod tests;

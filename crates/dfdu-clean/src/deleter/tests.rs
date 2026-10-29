use super::*;
use crossbeam_channel::Receiver;

fn plan_of(path: PathBuf, scan_root: PathBuf) -> Plan {
    let mut tree = dfdu_tree::Tree::new();
    let meta = dfdu_tree::Meta {
        kind: Kind::File,
        apparent: 1,
        alloc: 0,
        mtime: 0,
        dev: 0,
        ino: 0,
        nlink: 1,
        mount: 0,
    };
    let root = tree.new_root(b"/x", &meta, false);
    Plan {
        id: 1,
        mode: Mode::Permanent,
        scan_root: scan_root.into(),
        items: vec![PlanItem {
            node: tree.node_ref(root),
            path: path.into(),
            identity: Identity {
                dev: 0,
                ino: 0,
                kind: Kind::File,
                apparent: 1,
                mtime: 0,
            },
            bytes: 1,
            inside: Arc::default(),
        }],
        protected: None,
    }
}

/// The messages up to the plan's `Done`: its item results and any journal notes.
fn run(
    rx: &Receiver<DeleterMsg>,
    h: &DeleterHandle,
    plan: Plan,
) -> (Vec<DeleteResult>, Vec<String>) {
    h.reqs.send(DeleterReq::Plan(plan)).unwrap();
    let (mut results, mut notes) = (vec![], vec![]);
    loop {
        match rx.recv_timeout(Duration::from_secs(30)).unwrap() {
            DeleterMsg::Item { result, .. } => results.push(result),
            DeleterMsg::Unfinished { notes: n, .. } => notes.extend(n),
            DeleterMsg::Done { .. } => return (results, notes),
            _ => {}
        }
    }
}

#[test]
fn without_a_journal_nothing_is_deleted_and_the_cause_is_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(tmp.path()).unwrap();
    let victim = root.join("victim");
    fs::write(&victim, b"x").unwrap();

    // No journal directory at all: refused.
    let (tx, rx) = unbounded();
    let h = spawn_deleter(None, Duration::from_secs(60), tx);
    let (results, notes) = run(&rx, &h, plan_of(victim.clone(), root.clone()));
    assert_eq!(results, [DeleteResult::Refused(Refusal::NoJournal)]);
    assert!(notes.is_empty(), "{notes:?}");
    drop(h);

    // A journal directory that cannot be read or created (a file is in its place): the
    // startup scan's error is a note, and the item fails with why.
    let blocked = root.join("ops");
    fs::write(&blocked, b"").unwrap();
    let (tx, rx) = unbounded();
    let h = spawn_deleter(Some(blocked.clone()), Duration::from_secs(60), tx);
    let (results, notes) = run(&rx, &h, plan_of(victim.clone(), root.clone()));
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(
        notes[0].starts_with(&blocked.display().to_string()),
        "{notes:?}"
    );
    let [
        DeleteResult::Failed {
            partial: false,
            msg,
            ..
        },
    ] = &results[..]
    else {
        panic!("{results:?}");
    };
    assert!(
        msg.starts_with("the deletion journal could not be written: "),
        "{msg}"
    );
    assert!(victim.exists());
}

/// A result the journal cannot take is reported as a note; the item's own result stands.
#[test]
fn a_result_the_journal_cannot_record_is_noted() {
    let tmp = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(tmp.path()).unwrap();
    let path = root.join("session.jsonl");
    fs::write(&path, b"").unwrap();
    // Opened for reading only, so every write fails.
    let file = fs::File::open(&path).unwrap();
    let mut journal = Journal::resume(file, path);
    let mut stager = Stager::new(journal.session(), None);
    // Outside the scan root: refused before any intent, then its result is written.
    let plan = plan_of(root.join("victim"), root.join("scanned"));
    let mut notes = vec![];
    let (r, staged_at) = run_item(&mut journal, &mut stager, &plan, &plan.items[0], &mut notes);
    assert_eq!(r, DeleteResult::Refused(Refusal::OutsideScanRoot));
    assert_eq!(staged_at, None);
    let [note] = &notes[..] else {
        panic!("{notes:?}");
    };
    assert!(
        note.contains("victim: the journal could not record the result"),
        "{note}"
    );
}

/// A plan for another host's paths names nothing here: refused, even where its bytes would
/// name a file on this machine.
#[test]
fn a_plan_for_another_hosts_paths_is_refused() {
    use dfdu_tree::Host;
    let tmp = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(tmp.path()).unwrap();
    let victim = root.join("victim");
    fs::write(&victim, b"x").unwrap();
    let mut plan = plan_of(victim.clone(), root);
    let other = match Host::LOCAL {
        Host::Unix => Host::Windows,
        Host::Windows => Host::Unix,
    };
    let foreign = |p: &HostPath| HostPath::new(other, p.as_bytes().to_vec());
    plan.scan_root = foreign(&plan.scan_root);
    plan.items[0].path = foreign(&plan.items[0].path);
    let r = verify(&plan.items[0], &plan).map_err(DeleteResult::from);
    assert_eq!(r, Err(DeleteResult::Refused(Refusal::OutsideScanRoot)));
    assert!(victim.exists());
}

/// The protected paths a plan carries (its rule set's) are refused on top of this
/// process's: a file only they protect, and the home directory they are for, are kept,
/// and the same file is deleted by a plan without them.
#[cfg(unix)]
#[test]
fn the_plans_protected_paths_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(tmp.path()).unwrap();
    let home = root.join("home");
    let settings = home.join(".m2/settings.xml");
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    fs::write(&settings, b"x").unwrap();
    let env = dfdu_rules::Env::with(
        dfdu_rules::Platform::Linux,
        &[("HOME", home.to_str().unwrap())],
    );
    let protected = Arc::new(dfdu_rules::protected::Protected::new(&env));
    let m = dfdu_tree::Meta::from_metadata(&fs::symlink_metadata(&settings).unwrap());
    let real = |path: &Path| {
        let mut plan = plan_of(path.to_path_buf(), root.clone());
        plan.items[0].identity = Identity {
            dev: m.dev,
            ino: m.ino,
            kind: m.kind,
            apparent: m.apparent,
            mtime: m.mtime,
        };
        plan
    };

    let (tx, rx) = unbounded();
    let h = spawn_deleter(Some(root.join("journals")), Duration::from_secs(60), tx);
    for p in [&settings, &home] {
        let plan = Plan {
            protected: Some(protected.clone()),
            ..real(p)
        };
        let (results, _) = run(&rx, &h, plan);
        assert_eq!(
            results,
            [DeleteResult::Refused(Refusal::Protected)],
            "{}",
            p.display()
        );
    }
    assert!(settings.exists());
    let (results, _) = run(&rx, &h, real(&settings));
    assert_eq!(results, [DeleteResult::Deleted]);
    assert!(!settings.exists());
}

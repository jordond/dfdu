use super::*;
use crate::Identity;

#[test]
fn lines_carry_valid_crc_and_detect_damage() {
    let dir = tempfile::tempdir().unwrap();
    let mut j = Journal::create(dir.path()).unwrap();
    let mut tree = dfdu_tree::Tree::new();
    let meta = dfdu_tree::Meta {
        kind: Kind::Dir,
        apparent: 0,
        alloc: 0,
        mtime: 0,
        dev: 0,
        ino: 0,
        nlink: 1,
        mount: 0,
    };
    let root = tree.new_root(b"/x", &meta, true);
    let item = PlanItem {
        node: tree.node_ref(root),
        path: Path::new("/tmp/a \"quoted\"\nname").into(),
        identity: Identity {
            dev: 1,
            ino: 2,
            kind: Kind::File,
            apparent: 3,
            mtime: 4,
        },
        bytes: 4096,
        inside: std::sync::Arc::default(),
    };
    j.intent(7, Mode::Trash, &item, None).unwrap();
    j.result(7, &item, &DeleteResult::Deleted).unwrap();
    j.sync().unwrap();
    let path = j.path().to_path_buf();
    // Windows locks are mandatory: the file can be read only after the journal is closed.
    drop(j);
    let text = fs::read_to_string(&path).unwrap();
    let kinds: Vec<_> = text.lines().map(|l| check_line(l).unwrap()).collect();
    assert_eq!(kinds, ["session", "intent", "done"]);
    let damaged = text
        .lines()
        .nth(1)
        .unwrap()
        .replace("\"plan\":7", "\"plan\":8");
    assert!(check_line(&damaged).is_none());
    let torn = &text.lines().nth(2).unwrap()[..20];
    assert!(check_line(torn).is_none());
    // A field from a newer version: the CRC covers the bytes as written, not a re-encoding.
    let rec = r#"{"t":"session","pid":1,"host":"x"}"#;
    let newer = format!(
        r#"{{"rec":{rec},"crc":"{:08x}"}}"#,
        crc32c::crc32c(rec.as_bytes())
    );
    assert_eq!(check_line(&newer), Some("session"));
}

fn line_of(rec: &str) -> String {
    format!(
        r#"{{"rec":{rec},"crc":"{:08x}"}}"#,
        crc32c::crc32c(rec.as_bytes())
    )
}

/// Intent records as earlier versions wrote them parse to the same mode and kind, and are
/// written byte for byte as before.
#[test]
fn intent_mode_and_kind_keep_their_journal_names() {
    for (mode, name, kind, kind_name) in [
        (Mode::Trash, "trash", Kind::File, "file"),
        (Mode::Stage, "stage", Kind::Dir, "dir"),
        (Mode::Permanent, "delete", Kind::Symlink, "symlink"),
    ] {
        let rec = format!(
            r#"{{"t":"intent","plan":3,"path":"/x/a","path_hex":"2f782f61","dev":1,"ino":2,"kind":"{kind_name}","size":5,"mtime":6,"mode":"{name}","stage_hex":"2f73"}}"#
        );
        let line = line_of(&rec);
        let Some(Record::Intent {
            item,
            mode: m,
            stage_hex,
        }) = parse_line(&line)
        else {
            panic!("not an intent: {line}");
        };
        assert_eq!(m, mode);
        assert_eq!(m.journal_name(), name);
        assert_eq!(item.kind, kind);
        assert_eq!(stage_hex.as_deref(), Some("2f73"));
        let again = Record::Intent {
            item,
            mode: m,
            stage_hex,
        };
        assert_eq!(serde_json::to_string(&again).unwrap(), rec);
    }
    // A kind this version does not know is another kind; a mode it does not know makes the
    // record unreadable.
    let rec = r#"{"t":"done","plan":3,"path":"/x/a","path_hex":"2f782f61","dev":1,"ino":2,"kind":"fifo","size":5,"mtime":6}"#;
    let Some(Record::Done { item }) = parse_line(&line_of(rec)) else {
        panic!("not a done record");
    };
    assert_eq!(item.kind, Kind::Other);
    let rec = r#"{"t":"intent","plan":3,"path":"/x/a","path_hex":"2f782f61","dev":1,"ino":2,"kind":"file","size":5,"mtime":6,"mode":"shred"}"#;
    assert!(parse_line(&line_of(rec)).is_none());
}

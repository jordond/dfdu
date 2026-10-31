use super::*;
use crate::test_support::local_mount;

fn at(p: &str, network: bool) -> NotEntered {
    NotEntered {
        path: p.into(),
        network,
    }
}

#[test]
fn footer_lines_name_the_largest_and_count_the_rest() {
    let mut list = vec![
        (
            at("/mnt/data", false),
            Some(local_mount("zfs", Some(3 << 40))),
        ),
        (at("/nas", true), Some(local_mount("nfs4", None))),
        (
            at("/boot", false),
            Some(local_mount("ext4", Some(300 << 20))),
        ),
    ];
    assert_eq!(
        not_entered_lines(&list, None),
        [
            "not entered: /mnt/data (zfs, 3.0 TiB used), /boot (ext4, 300.0 MiB used); \
             --cross-mounts to include",
            "not entered: /nas (nfs4); --network to include",
        ]
    );
    for i in 0..6 {
        list.push((
            at(&format!("/snap/x/{i}"), false),
            Some(local_mount("squashfs", Some(i))),
        ));
    }
    let lines = not_entered_lines(&list, None);
    assert!(
        lines[0].ends_with(" and 3 more; --cross-mounts to include"),
        "{lines:?}"
    );
    assert!(
        lines[0].starts_with("not entered: /mnt/data (zfs"),
        "{lines:?}"
    );
    // Not looked up (a remote scan): the paths alone.
    let home = Path::new("/home/u");
    let bare = [(at("/home/u/mnt", false), None)];
    assert_eq!(
        not_entered_lines(&bare, Some(home)),
        ["not entered: ~/mnt; --cross-mounts to include"]
    );
}

#[test]
fn link_notes_name_the_rules_and_where_to_scan() {
    let env = dfdu_rules::Env::with(dfdu_rules::Platform::Linux, &[("HOME", "/home/u")]);
    let (rules, _) = dfdu_rules::builtin(&env);
    let ix = |id: &str| rules.index(id).expect(id);
    let l = Linked {
        link: "/home/u/.cache".into(),
        target: "/mnt/data/cache".into(),
        rules: vec![ix("generic.cachedir-tag")],
    };
    let note = linked_note(&rules, &l, Some(Path::new("/home/u")), "dfdu report");
    let name = &rules.rule(ix("generic.cachedir-tag")).name;
    assert_eq!(
        note,
        format!(
            "~/.cache is a link to /mnt/data/cache ({name}), not scanned: \
             dfdu report /mnt/data/cache"
        )
    );
}

#[test]
fn more_links_counts_the_links_not_named() {
    assert_eq!(more_links(5, 5), None);
    assert_eq!(more_links(2, 5), None);
    assert_eq!(
        more_links(6, 5).as_deref(),
        Some("and 1 more link out of the scan")
    );
    assert_eq!(
        more_links(8, 5).as_deref(),
        Some("and 3 more links out of the scan")
    );
}

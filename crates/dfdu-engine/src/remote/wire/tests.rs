use std::sync::Arc;

use crate::classifier::{CheckMsg, CheckReq};
use crate::extents::Measurement;
use crate::measure::{MeasureFile, MeasureMsg, MeasureReq, MeasureResult};
use dfdu_clean::inside::{Listing, Listings};
use dfdu_clean::{DeleteResult, DeleterMsg, DeleterReq, Identity, Mode, Plan, PlanItem, Refusal};
use dfdu_rules::protected::State;
use dfdu_rules::{Hit, Inside, RuleIx, RuleSet, Safety};
use dfdu_tree::{Class, ErrKind, Kind, Meta, NodeRef, Subvol};

use dfdu_scan::{
    Chunk, ChunkStatus, DirId, Entry, EntryError, FileRec, JobId, ListingStatus, Mark, NameSpan,
    Probe, ScanMsg, SkipReason, SubdirRec,
};

use super::scan::{dec_class, enc_class};
use super::*;

/// The built-in rules for Linux, and the number of one with a `degrade` condition.
fn rules() -> (RuleSet, RuleIx) {
    let env = dfdu_rules::Env::with(dfdu_rules::Platform::Linux, &[("HOME", "/home/u")]);
    let set = dfdu_rules::builtin(&env).0;
    let nm = set.index("node.node_modules").or_else(|| {
        let i = set.rules().iter().position(|r| !r.degrade.is_empty())?;
        Some(RuleIx(u16::try_from(i).unwrap()))
    });
    (set, nm.unwrap())
}

fn meta(kind: Kind, n: u64) -> Meta {
    Meta {
        kind,
        apparent: n,
        alloc: n.wrapping_mul(4096),
        mtime: n.cast_signed().wrapping_mul(-1_000_003),
        dev: 7,
        ino: n << 40,
        nlink: 3,
        mount: 0,
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "a sample of every message from the agent"
)]
fn sample() -> Vec<FromAgent> {
    let rule = rules().1;
    let names = b"one\xfftwosub".to_vec();
    let chunk = Chunk {
        job: JobId(2),
        dir: DirId(9),
        seq: 70,
        root_meta: Some(meta(Kind::Dir, 1)),
        probe: Some(Box::new(Probe {
            subvol: Some(Subvol {
                id: 256,
                uuid: [3; 16],
                parent_uuid: Some([4; 16]),
                read_only: true,
                otime: -5,
                qgroup: Some((1 << 33, 12)),
            }),
            shares_extents: true,
            profile: Some((dfdu_sys::Profile::Raid1c3, 3.0)),
        })),
        names,
        files: vec![
            FileRec {
                name: NameSpan { off: 0, len: 4 },
                meta: meta(Kind::File, 10),
            },
            FileRec {
                name: NameSpan { off: 4, len: 3 },
                meta: meta(Kind::Symlink, u64::MAX),
            },
        ],
        first_subdir: DirId(40),
        // One of each skip reason.
        subdirs: [
            None,
            Some(SkipReason::DuplicateMount),
            Some(SkipReason::OtherFilesystem),
            Some(SkipReason::Network),
            Some(SkipReason::Kernel),
            Some(SkipReason::Excluded),
            Some(SkipReason::Kept),
        ]
        .into_iter()
        .enumerate()
        .map(|(i, skip)| SubdirRec {
            name: NameSpan { off: 7, len: 3 },
            meta: meta(Kind::Dir, 2 + i as u64),
            mount_root: skip.is_some(),
            skip,
        })
        .collect(),
        errors: vec![EntryError {
            name: NameSpan { off: 0, len: 0 },
            err: ErrKind::Os(-22),
        }],
        marks: vec![
            (
                Entry::Subdir(0),
                Mark::Class(Class {
                    rule,
                    safety: Safety::Review,
                    degraded: Some(0),
                    flags: 0,
                }),
            ),
            (Entry::File(1), Mark::Excepted),
        ],
        status: ChunkStatus::Panicked("boom".into()),
    };
    vec![
        FromAgent::Hello {
            min: 1,
            max: 1,
            tool: "dfdu 0.1.0".into(),
        },
        FromAgent::Batch(vec![
            ScanMsg::Chunk(chunk),
            ScanMsg::Close {
                job: JobId(2),
                dir: DirId(9),
                chunks: 1,
                status: ListingStatus::Partial {
                    panicked: 1,
                    cancelled: 0,
                    read_error: Some(ErrKind::PermissionDenied),
                },
                classes: vec![(
                    DirId(40),
                    Class {
                        rule: RuleIx(0),
                        safety: Safety::Regenerable,
                        degraded: None,
                        flags: 0,
                    },
                )],
            },
            ScanMsg::Stalled {
                job: JobId(2),
                dir: DirId(40),
            },
            ScanMsg::Quiescent {
                submitted_seq: u64::MAX,
            },
        ]),
        FromAgent::Heartbeat,
        FromAgent::Started {
            job: JobId(2),
            seq: 17,
        },
        FromAgent::Abandoned {
            job: JobId(2),
            dir: DirId(5),
            complete: false,
        },
        FromAgent::Fatal("gone".into()),
        FromAgent::Deleter(DeleterMsg::Item {
            plan: 3,
            node: NodeRef::from_bits(u64::MAX - 9),
            bytes: 4096,
            result: DeleteResult::Failed {
                partial: true,
                err: ErrKind::Os(13),
                msg: "denied".into(),
            },
        }),
        FromAgent::Deleter(DeleterMsg::Item {
            plan: 3,
            node: NodeRef::from_bits(1),
            bytes: 0,
            result: DeleteResult::Refused(Refusal::TrashOtherSubvolume("/sv".into())),
        }),
        FromAgent::Deleter(DeleterMsg::Done {
            plan: 3,
            btrfs: true,
        }),
        FromAgent::Deleter(DeleterMsg::Restored {
            plan: 3,
            original: "/a/\u{e9}".into(),
            staged: true,
            bytes: 9,
            result: Ok("/a/\u{e9}.restored-1".into()),
        }),
        FromAgent::Deleter(DeleterMsg::Committed {
            plan: 3,
            bytes: 9,
            result: Err("busy".into()),
        }),
        FromAgent::Checked(vec![
            CheckMsg::Guards {
                node: NodeRef::from_bits(5),
                rule,
                tracked: Some(true),
                running: Some("Xcode".into()),
            },
            CheckMsg::Guards {
                node: NodeRef::from_bits(6),
                rule,
                tracked: None,
                running: None,
            },
            CheckMsg::NativeSize {
                rule,
                result: Ok(1 << 40),
            },
        ]),
        FromAgent::Deleter(DeleterMsg::Unfinished {
            items: vec!["x".into()],
            notes: vec![],
        }),
        FromAgent::Rules {
            platform: "linux".into(),
            vars: vec![("HOME".into(), "/home/u".into())],
            links: vec![(
                "/home/u/.gradle/caches".into(),
                "/mnt/data/gradle/caches".into(),
            )],
            fingerprint: u64::MAX - 3,
            root: class(rule),
        },
        FromAgent::Host {
            measure_min: 1 << 20,
            privileged: true,
            host: dfdu_tree::Host::Windows,
            root: b"C:\\Users\\u".to_vec(),
        },
        FromAgent::Measured(vec![
            MeasureMsg {
                round: 3,
                node: NodeRef::from_bits(42 << 32 | 3),
                inode: (7, u64::MAX),
                result: MeasureResult::Measured(Measurement {
                    fs: [9; 16],
                    excl: 4096,
                    compressed: true,
                    ranges: vec![(0, 8192), (1 << 40, 1 << 41)],
                    points: vec![(77, 4096)],
                    exact: false,
                }),
            },
            MeasureMsg {
                round: 3,
                node: NodeRef::from_bits(5),
                inode: (7, 8),
                result: MeasureResult::Changed,
            },
            MeasureMsg {
                round: 0,
                node: NodeRef::from_bits(6),
                inode: (7, 9),
                result: MeasureResult::Failed(ErrKind::Os(5)),
            },
        ]),
    ]
}

fn measure_req() -> MeasureReq {
    MeasureReq {
        round: 2,
        dir: "/srv/\u{e9}t\u{e9}".into(),
        files: vec![
            MeasureFile {
                node: NodeRef::from_bits(42 << 32 | 3),
                name: b"a\xffb".to_vec(),
                inode: (1, 2),
            },
            MeasureFile {
                node: NodeRef::from_bits(0),
                name: b"c".to_vec(),
                inode: (u64::MAX, 0),
            },
        ],
    }
}

pub(super) fn plan() -> Plan {
    let mut inside = Listings::default();
    inside.insert(
        "".into(),
        Listing {
            ino: 5,
            entries: vec![(u64::MAX, 0, Kind::Dir), (1, 77, Kind::File)],
        },
    );
    inside.insert("sub/\u{e9}".into(), Listing::default());
    Plan {
        id: 7,
        mode: Mode::Stage,
        scan_root: "/home/u".into(),
        items: vec![PlanItem {
            node: NodeRef::from_bits(42 << 32 | 3),
            path: "/home/u/cache".into(),
            identity: Identity {
                dev: 1,
                ino: 2,
                kind: Kind::Dir,
                apparent: 3,
                mtime: -4,
            },
            bytes: 8192,
            inside: Arc::new(inside),
        }],
        protected: None,
    }
}

fn class(rule: RuleIx) -> WireClass {
    WireClass {
        path: b"/home/u/\xffproj".to_vec(),
        inside: vec![RuleIx(0), rule],
        protection: State::Sticky,
        chain: vec![Hit {
            rule,
            safety: Safety::Review,
            degraded: Some(0),
            inside: Inside::Check,
            open: true,
        }],
    }
}

/// Every message reads back as written (compared by writing it again).
#[test]
fn messages_round_trip() {
    let (set, rule) = rules();
    for up in sample() {
        let bytes = up.encode();
        let back = FromAgent::decode(&bytes, Some(&set), dfdu_tree::Host::LOCAL).unwrap();
        assert_eq!(back.encode(), bytes, "{up:?}");
    }
    let downs = [
        ToAgent::Hello {
            version: 1,
            root: b"/home/u".to_vec(),
        },
        ToAgent::Rules {
            files: vec![
                (None, "[[rule]]".into()),
                (
                    Some("/home/u/.config/dfdu/rules/a.toml".into()),
                    String::new(),
                ),
            ],
            root: b"/srv/\xffdata".to_vec(),
        },
        ToAgent::StartJob {
            job: JobId(1),
            root: b"/srv/\xffdata".to_vec(),
            cross_mounts: true,
            network: false,
            enter: vec![(2, 7), (u64::MAX, 1 << 40)],
            class: None,
        },
        ToAgent::StartJob {
            job: JobId(1),
            root: b"/".to_vec(),
            cross_mounts: false,
            network: true,
            enter: vec![],
            class: Some(class(rule)),
        },
        ToAgent::Delete(DeleterReq::Plan(plan())),
        ToAgent::Delete(DeleterReq::Undo { plan: 7 }),
        ToAgent::Delete(DeleterReq::Commit),
        ToAgent::AbortDelete,
        ToAgent::Check(vec![CheckReq::Guards {
            node: NodeRef::from_bits(9),
            rule,
            path: "/srv/web/node_modules".into(),
            vcs: true,
            processes: vec!["node".into(), String::new()],
        }]),
        ToAgent::Measure(vec![measure_req()]),
        ToAgent::EndJob { job: JobId(1) },
        ToAgent::StopDirs {
            job: JobId(1),
            dirs: vec![DirId(3), DirId(u32::MAX)],
        },
        ToAgent::Abandon {
            job: JobId(1),
            dir: DirId(3),
        },
    ];
    for d in downs {
        let bytes = d.encode();
        assert_eq!(ToAgent::decode(&bytes, Some(&set)).unwrap().encode(), bytes);
    }
    // A class before the rules, or naming a rule the agent does not have, is refused.
    let job = ToAgent::StartJob {
        job: JobId(1),
        root: b"/".to_vec(),
        cross_mounts: false,
        network: false,
        enter: vec![],
        class: Some(class(rule)),
    };
    assert!(ToAgent::decode(&job.encode(), None).is_err());
    let few = dfdu_rules::load_from(
        &dfdu_rules::Env::with(dfdu_rules::Platform::Linux, &[]),
        &[],
    )
    .0;
    assert!(ToAgent::decode(&job.encode(), Some(&few)).is_err());
}

/// A class is written as the rule's index (a varint), the safety level's number, the
/// `degrade` condition and the flags; a safety number past the last level is refused.
/// Paths from the agent are on its host, whatever this one is: a Unix host's name holding `\`
/// or bytes that are not UTF-8 comes back as sent, and a Windows host's path keeps its
/// separators, tagged with the host the local side was told of.
#[test]
fn paths_come_back_on_the_agents_host() {
    use dfdu_tree::{Host, HostPath};
    for (host, original) in [
        (Host::Unix, &b"/home/u/a\\b/caf\xe9"[..]),
        (Host::Windows, b"C:\\Users\\u\\proj"),
    ] {
        let original = HostPath::new(host, original.to_vec());
        let up = FromAgent::Deleter(DeleterMsg::Restored {
            plan: 1,
            original: original.clone(),
            staged: false,
            bytes: 1,
            result: Ok(original.join(b"x")),
        });
        let FromAgent::Deleter(DeleterMsg::Restored {
            original: o,
            result: Ok(r),
            ..
        }) = FromAgent::decode(&up.encode(), None, host).unwrap()
        else {
            panic!("not a restore");
        };
        assert_eq!(o, original);
        assert_eq!((r.host(), r.file_name()), (host, Some(&b"x"[..])));
        assert_eq!(r.parent(), Some(original));
    }
}

#[test]
fn class_layout() {
    let c = Class {
        rule: RuleIx(300),
        safety: Safety::Review,
        degraded: Some(1),
        flags: Class::CHECKED,
    };
    let mut e = Enc::default();
    enc_class(&mut e, c);
    assert_eq!(e.0, [0xac, 0x02, 2, 1, 1, 2]);
    let (set, rule) = rules();
    let mut e = Enc::default();
    enc_class(
        &mut e,
        Class {
            rule,
            degraded: None,
            ..c
        },
    );
    let back = dec_class(&mut Dec::new(&e.0), Some(&set)).unwrap();
    assert_eq!((back.rule, back.safety), (rule, Safety::Review));
    let safety_at = e.0.len() - 3;
    e.0[safety_at] = u8::try_from(Safety::ALL.len()).unwrap();
    assert!(dec_class(&mut Dec::new(&e.0), Some(&set)).is_err());
}

/// Kinds go as one byte, the same in metadata and in job answers; an unknown byte is an error.
#[test]
fn kind_layout() {
    let kinds = [Kind::File, Kind::Dir, Kind::Symlink, Kind::Other];
    let mut e = Enc::default();
    for k in kinds {
        e.kind(k);
    }
    assert_eq!(e.0, [0, 1, 2, 3]);
    let mut d = Dec::new(&e.0);
    for k in kinds {
        assert_eq!(d.kind().unwrap(), k);
    }
    assert!(matches!(
        Dec::new(&[4]).kind(),
        Err(WireError::Malformed("kind"))
    ));
}

/// Frames carry their payload; a flipped bit is a checksum error, a cut frame an error,
/// and an empty stream the end.
#[test]
fn frames_are_checked() {
    let mut out = Vec::new();
    for up in sample() {
        write_frame(&mut out, &up.encode()).unwrap();
    }
    let mut r = &out[..];
    let mut buf = Vec::new();
    let mut n = 0;
    let set = rules().0;
    while read_frame(&mut r, &mut buf).unwrap() {
        FromAgent::decode(&buf, Some(&set), dfdu_tree::Host::LOCAL).unwrap();
        n += 1;
    }
    assert_eq!(n, sample().len());
    let mut bad = out.clone();
    bad[20] ^= 1;
    let mut r = &bad[..];
    let errs = (0..n)
        .filter(|_| read_frame(&mut r, &mut buf).is_err())
        .count();
    assert!(errs >= 1);
    assert!(read_frame(&mut &out[..out.len() - 1], &mut buf).is_ok());
    let mut r = &out[..9];
    assert!(read_frame(&mut r, &mut buf).is_err());
    let mut huge = u32::try_from(MAX_FRAME + 1).unwrap().to_le_bytes().to_vec();
    huge.extend([0; 4]);
    assert!(matches!(
        read_frame(&mut &huge[..], &mut buf),
        Err(WireError::TooLarge(_))
    ));
}

/// No cut or bit flip of a payload panics, and what decodes is self-consistent: spans in
/// range, marks naming entries, classes naming known rules.
#[test]
fn damaged_payloads_never_panic() {
    let set = rules().0;
    for up in sample() {
        let bytes = up.encode();
        for cut in 0..bytes.len() {
            assert!(FromAgent::decode(&bytes[..cut], Some(&set), dfdu_tree::Host::LOCAL).is_err());
        }
        for i in 0..bytes.len() * 8 {
            let mut b = bytes.clone();
            b[i / 8] ^= 1 << (i % 8);
            if let Ok(FromAgent::Batch(msgs)) =
                FromAgent::decode(&b, Some(&set), dfdu_tree::Host::LOCAL)
            {
                for m in msgs {
                    if let ScanMsg::Chunk(c) = m {
                        // Reading names must not panic.
                        for f in &c.files {
                            let _ = c.name(f.name);
                        }
                        for s in &c.subdirs {
                            let _ = c.name(s.name);
                        }
                    }
                }
            }
        }
    }
    // Classes of rules the local side does not have are refused, and any without rules.
    let batch = &sample()[1];
    assert!(FromAgent::decode(&batch.encode(), None, dfdu_tree::Host::LOCAL).is_err());
    let few = dfdu_rules::load_from(
        &dfdu_rules::Env::with(dfdu_rules::Platform::Linux, &[]),
        &[],
    )
    .0;
    assert!(FromAgent::decode(&batch.encode(), Some(&few), dfdu_tree::Host::LOCAL).is_err());
    for d in [
        ToAgent::Rules {
            files: vec![(Some("a".into()), "x".into())],
            root: b"/".to_vec(),
        },
        ToAgent::Delete(DeleterReq::Plan(plan())),
        ToAgent::Measure(vec![measure_req()]),
    ] {
        let bytes = d.encode();
        for cut in 0..bytes.len() {
            assert!(ToAgent::decode(&bytes[..cut], Some(&set)).is_err());
        }
    }
    // A measured name is opened relative to its directory: no paths.
    for name in [&b"a/b"[..], b"..", b".", b""] {
        let mut r = measure_req();
        r.files[1].name = name.to_vec();
        assert!(ToAgent::decode(&ToAgent::Measure(vec![r]).encode(), None).is_err());
    }
    // A shared range cannot end before it starts.
    let mut m = sample().pop().unwrap();
    if let FromAgent::Measured(msgs) = &mut m
        && let MeasureResult::Measured(me) = &mut msgs[0].result
    {
        me.ranges[0] = (9, 8);
    }
    assert!(FromAgent::decode(&m.encode(), None, dfdu_tree::Host::LOCAL).is_err());
}

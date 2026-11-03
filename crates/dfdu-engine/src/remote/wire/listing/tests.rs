use dfdu_tree::Host;

use super::super::{FromAgent, ToAgent};
use super::*;

fn listed(entries: Vec<ListEntry>) -> Vec<u8> {
    FromAgent::Listed {
        id: 1,
        result: Ok(entries),
    }
    .encode()
}

fn entry(name: &[u8]) -> ListEntry {
    ListEntry {
        name: name.to_vec(),
        dir: true,
    }
}

fn decode_up(bytes: &[u8], host: Host) -> Result<FromAgent, super::super::WireError> {
    FromAgent::decode(bytes, None, host)
}

#[test]
fn a_listing_of_the_most_entries_is_read_and_one_more_is_refused() {
    let full: Vec<ListEntry> = (0..MAX_ENTRIES)
        .map(|i| entry(format!("d{i}").as_bytes()))
        .collect();
    let Ok(FromAgent::Listed {
        result: Ok(back), ..
    }) = decode_up(&listed(full.clone()), Host::Unix)
    else {
        panic!("a full listing is read");
    };
    assert_eq!(back, full);
    let mut over = full;
    over.push(entry(b"one-more"));
    assert!(decode_up(&listed(over), Host::Unix).is_err());
}

#[test]
fn names_that_are_too_long_or_not_one_component_are_refused() {
    assert!(decode_up(&listed(vec![entry(&[b'a'; MAX_NAME])]), Host::Unix).is_ok());
    for bad in [&[b'a'; MAX_NAME + 1][..], b"", b".", b"..", b"a/b", b"a\0b"] {
        assert!(
            decode_up(&listed(vec![entry(bad)]), Host::Unix).is_err(),
            "{bad:?}"
        );
    }
    // `\` is a name's byte on a Unix host and a separator on a Windows one.
    assert!(decode_up(&listed(vec![entry(b"a\\b")]), Host::Unix).is_ok());
    assert!(decode_up(&listed(vec![entry(b"a\\b")]), Host::Windows).is_err());
}

#[test]
fn mounts_and_errors_past_their_bounds_are_refused() {
    let mount = |point: &[u8], fstype: &str| HostMount {
        point: point.to_vec(),
        fstype: fstype.into(),
        used: None,
        total: Some(1),
    };
    let up = |mounts: Vec<HostMount>| {
        FromAgent::Mounts {
            id: 2,
            result: Ok(mounts),
        }
        .encode()
    };
    assert!(decode_up(&up(vec![mount(b"/", "apfs")]), Host::Unix).is_ok());
    assert!(decode_up(&up(vec![mount(b"", "apfs")]), Host::Unix).is_err());
    let long = "x".repeat(MAX_FSTYPE + 1);
    assert!(decode_up(&up(vec![mount(b"/", &long)]), Host::Unix).is_err());
    let many = vec![mount(b"/", "x"); MAX_MOUNTS + 1];
    assert!(decode_up(&up(many), Host::Unix).is_err());

    let err = |text: String| {
        FromAgent::Listed {
            id: 3,
            result: Err(text),
        }
        .encode()
    };
    assert!(decode_up(&err("x".repeat(MAX_ERROR + 1)), Host::Unix).is_err());
    // The agent clips what it sends to what is read.
    let clipped = clip_error("é".repeat(MAX_ERROR));
    assert!(clipped.len() <= MAX_ERROR);
    assert!(decode_up(&err(clipped), Host::Unix).is_ok());
}

#[test]
fn a_path_to_list_decodes_even_when_the_agent_cannot_list_it() {
    // The agent answers these with why; refusing them here would end the link.
    for path in [vec![b'/'; MAX_PATH], vec![b'/'; MAX_PATH + 1], Vec::new()] {
        let down = ToAgent::List {
            id: 1,
            path: path.clone(),
        };
        let got = ToAgent::decode(&down.encode(), None).unwrap();
        assert!(matches!(got, ToAgent::List { id: 1, path: p } if p == path));
    }
}

#[test]
fn unknown_tags_and_damaged_answers_are_refused() {
    // One past the last tag on each side.
    assert!(decode_up(&[13], Host::Unix).is_err());
    assert!(ToAgent::decode(&[12, 0], None).is_err());
    // A result tag that is neither a value nor an error.
    let mut b = listed(vec![entry(b"a")]);
    b[2] = 2;
    assert!(decode_up(&b, Host::Unix).is_err());
    let up = listed(vec![entry(b"a"), entry(b"b")]);
    for cut in 0..up.len() {
        assert!(decode_up(&up[..cut], Host::Unix).is_err());
    }
    let down = ToAgent::List {
        id: 300,
        path: b"/srv".to_vec(),
    }
    .encode();
    for cut in 0..down.len() {
        assert!(ToAgent::decode(&down[..cut], None).is_err());
    }
}

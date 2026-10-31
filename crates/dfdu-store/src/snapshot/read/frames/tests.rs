use super::*;

fn rec(b: &[u8]) -> Rec<&[u8]> {
    Rec(b)
}

fn msg(e: ImportError) -> String {
    match e {
        ImportError::Format { msg, .. } | ImportError::Limit(msg) => msg,
        ImportError::Io(e) => e.to_string(),
    }
}

#[test]
fn varints_read_little_end_first_and_signed_ones_zigzagged() {
    assert_eq!(rec(&[0]).u().unwrap(), 0);
    assert_eq!(rec(&[0x7f]).u().unwrap(), 127);
    assert_eq!(rec(&[0xac, 0x02]).u().unwrap(), 300);
    let mut max = vec![0xff; 9];
    max.push(0x01);
    assert_eq!(rec(&max).u().unwrap(), u64::MAX);
    for (b, v) in [(0, 0), (1, -1), (2, 1), (3, -2)] {
        assert_eq!(rec(&[b]).i().unwrap(), v);
    }
    assert_eq!(rec(&max).i().unwrap(), i64::MIN);
}

#[test]
fn a_number_past_ten_bytes_or_cut_short_is_refused() {
    let long = [0x80; 10];
    assert_eq!(msg(rec(&long).u().unwrap_err()), "a number too long");
    assert_eq!(msg(rec(&[0x80]).u().unwrap_err()), "the records end early");
}

#[test]
fn byte_strings_hold_to_their_limit() {
    assert_eq!(rec(&[3, b'a', b'b', b'c']).bytes(3).unwrap(), b"abc");
    assert!(matches!(
        rec(&[4, b'a', b'b', b'c', b'd']).bytes(3),
        Err(ImportError::Limit(_))
    ));
    assert_eq!(
        msg(rec(&[3, b'a']).bytes(3).unwrap_err()),
        "the records end early"
    );
}

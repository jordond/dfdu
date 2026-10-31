use super::*;

fn r(s: &[u8]) -> Reader<&[u8]> {
    Reader::new(s)
}

#[test]
fn strings_keep_raw_bytes_and_decode_escapes() {
    assert_eq!(r(b"\"a\xffb\"").string(100).unwrap(), b"a\xffb");
    assert_eq!(
        r(br#""\"\\\/\n\u00e9\ud83d\ude00""#).string(100).unwrap(),
        "\"\\/\né😀".as_bytes()
    );
    assert!(r(br#""\ud83d""#).string(100).is_err());
    assert!(r(br#""\ude00x""#).string(100).is_err());
    assert!(matches!(
        r(b"\"abcdef\"").string(3),
        Err(ImportError::Limit(_))
    ));
    assert!(r(b"\"abc").string(100).is_err());
}

#[test]
fn numbers_and_skipping() {
    assert_eq!(r(b"12").number().unwrap(), Num::U(12));
    assert_eq!(r(b"-3").number().unwrap(), Num::I(-3));
    assert_eq!(r(b"1.5e3").number().unwrap().as_u64(), Some(1500));
    assert!(r(b"-").number().is_err());
    let mut x = r(br#"{"a":[1,{"b":null}],"c":"d"} 7"#);
    x.skip(3).unwrap();
    assert_eq!(x.number().unwrap(), Num::U(7));
    assert!(matches!(
        r(b"[[[[1]]]]").skip(3),
        Err(ImportError::Limit(_))
    ));
}

#[test]
fn written_strings_read_back() {
    let name = b"q\"\\\n\x01\x7f\xff\xc3\xa9";
    let mut out = vec![];
    write_str(&mut out, name);
    assert_eq!(r(&out).string(100).unwrap(), name);
}

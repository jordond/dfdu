use super::*;

#[test]
fn counts_take_the_word_for_that_many() {
    assert_eq!(plural(1usize, "item", "items"), "1 item");
    assert_eq!(plural(0usize, "item", "items"), "0 items");
    assert_eq!(plural(3u64, "match", "matches"), "3 matches");
}

#[test]
fn base64_matches_rfc4648() {
    for (raw, enc) in [
        ("", ""),
        ("f", "Zg=="),
        ("fo", "Zm8="),
        ("foo", "Zm9v"),
        ("foob", "Zm9vYg=="),
        ("fooba", "Zm9vYmE="),
        ("foobar", "Zm9vYmFy"),
    ] {
        assert_eq!(base64(raw.as_bytes()), enc);
    }
    assert_eq!(base64(&[0xff, 0xfe, 0x80]), "//6A");
}

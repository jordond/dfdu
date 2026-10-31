use super::*;

#[test]
fn sizes_and_ages_parse() {
    assert_eq!(size_arg("100M"), Ok(100 << 20));
    assert_eq!(size_arg("1.5GiB"), Ok(3 << 29));
    assert_eq!(size_arg("500 KB"), Ok(500_000));
    assert_eq!(size_arg("0"), Ok(0));
    assert!(size_arg("lots").is_err());
    assert_eq!(age_arg("30d"), Ok(30 * 86_400));
    assert_eq!(age_arg("2w"), Ok(14 * 86_400));
    assert_eq!(age_arg("12h"), Ok(12 * 3_600));
    assert_eq!(age_arg("3"), Ok(3 * 86_400));
    assert!(age_arg("3y").is_err() && age_arg("d").is_err());
    assert!(category_arg("dev").is_ok() && category_arg("dev.build").is_ok());
    assert!(category_arg("de").is_err() && category_arg("build-cache").is_err());
}

/// A refused value says what was given and what is expected: clap prints it after the flag.
#[test]
fn refused_values_say_what_is_expected() {
    let msg = |e: BadValue| e.to_string();
    assert_eq!(
        size_arg("lots").map_err(msg),
        Err("`lots` is not a size (100M, 1.5GiB, 500KB)".into())
    );
    assert_eq!(
        age_arg("99999999999999999").map_err(msg),
        Err("`99999999999999999` is too long".into())
    );
    assert_eq!(safety_arg("review"), Ok(Safety::Review));
    let risky = safety_arg("risky").map_err(msg).unwrap_err();
    assert!(
        risky.starts_with("`risky` is not a safety level"),
        "{risky}"
    );
    assert_eq!(seconds("1.5"), Ok(1.5));
    assert!(seconds("-1").is_err() && seconds("40000000").is_err());
    assert_eq!(
        type_filter("").map_err(msg),
        Err("name at least one type or extension".into())
    );
    let boxed: Box<dyn std::error::Error + Send + Sync> = BadValue::new("no").into();
    assert_eq!(boxed.to_string(), "no");
}

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

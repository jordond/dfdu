//! The rule loader on a user's rule file: errors are reported, never a panic.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let env = dfdu_rules::Env::current();
    let file = (
        dfdu_rules::Source::File("fuzz.toml".into()),
        text.to_owned(),
    );
    let _ = dfdu_rules::load_from(&env, &[file]);
});

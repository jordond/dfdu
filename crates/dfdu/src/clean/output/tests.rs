use super::*;

/// A real run counts what it deleted and what failed; a dry run what it would delete.
#[test]
fn the_summary_counts_what_was_done_or_would_be() {
    let failed = Outcome::Failed("busy".into());
    let rows = [
        (1024, &Outcome::Deleted),
        (2048, &Outcome::Deleted),
        (4096, &failed),
        (8192, &Outcome::Planned),
    ];
    assert_eq!(
        summary(true, ModeArg::Trash, &rows),
        format!(
            "2 items ({}) moved to the trash, 1 failed",
            dfdu_engine::human(3072)
        )
    );
    assert_eq!(
        summary(false, ModeArg::Delete, &rows),
        format!(
            "dry run: 1 item ({}) would be deleted; nothing was changed (--yes does it)",
            dfdu_engine::human(8192)
        )
    );
    assert_eq!(
        summary(true, ModeArg::Stage, &[]),
        format!("0 items ({}) deleted", dfdu_engine::human(0))
    );
}

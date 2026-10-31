//! What the decoder targets share. Not a target itself: each includes it with `mod common;`.

/// The decoders' limits, with an entry cap low enough that a small input claiming a huge tree
/// is refused rather than allocated.
pub fn limits() -> dfdu_store::Limits {
    dfdu_store::Limits {
        max_entries: 200_000,
        ..Default::default()
    }
}

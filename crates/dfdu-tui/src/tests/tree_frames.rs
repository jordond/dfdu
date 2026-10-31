//! Frames of tree mode at the widths of the approved mockups: `~/dev` with `dfdu/`,
//! `dfdu/.claude/worktrees/` and `dfdu/target/` (a Rust build directory) expanded, small
//! entries folded, and `target/` selected, drawn at 120 and 80 columns.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use dfdu_tree::{Class, Ix};
use ratatui::style::Modifier;

use super::tree::{Fx, sync};
use super::{render, render_buffer, row_names, style_at};
use crate::app::App;
use crate::state::tree::TreeState;
use crate::theme::Theme;

const GIB: f64 = (1u64 << 30) as f64;

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "test sizes are small positive numbers of GiB"
)]
fn gib(n: f64) -> u64 {
    (n * GIB) as u64
}

/// A directory `name` in `parent` holding one file of `size` GiB.
fn filled(fx: &mut Fx, parent: Ix, name: &str, size: f64) -> Ix {
    let d = fx.dir(parent, name);
    fx.file(d, "data", gib(size));
    d
}

/// `~/dev` as the mockups show it, in tree mode with the selection on `dfdu/target/`.
fn dev(t0: Instant) -> App {
    let mut fx = Fx::at("/home/jo/dev");
    let env = dfdu_rules::Env::with(dfdu_rules::Platform::Linux, &[("HOME", "/home/jo")]);
    let rules = dfdu_rules::builtin(&env).0;
    let rule = rules.index("rust.target").expect("rule");
    fx.m.rules = Some(Arc::new(rules));

    let dfdu = fx.dir(fx.root, "dfdu");
    let claude = fx.dir(dfdu, ".claude");
    let worktrees = fx.dir(claude, "worktrees");
    for (name, size) in [
        ("desloppify-code-health", 7.4),
        ("fsevents-rescan", 6.9),
        ("apfs-labels", 5.8),
    ] {
        filled(&mut fx, worktrees, name, size);
    }
    for i in 0..4 {
        filled(&mut fx, worktrees, &format!("wt-{i}"), 0.17);
    }
    let target = fx.dir(dfdu, "target");
    fx.m.tree.set_class(
        target,
        Some(Class {
            rule,
            safety: dfdu_tree::Safety::Regenerable,
            degraded: None,
            flags: dfdu_tree::ClassFlags::NONE,
        }),
    );
    for (name, size) in [
        ("debug", 9.1),
        ("release", 2.2),
        ("x86_64-pc-windows-msvc", 1.7),
        ("llvm-cov-target", 1.6),
        ("bench-1m", 1.4),
    ] {
        filled(&mut fx, target, name, size);
    }
    for i in 0..9 {
        filled(&mut fx, target, &format!("tmp-{i}"), 0.144);
    }
    filled(&mut fx, dfdu, "fuzz", 2.5);
    for i in 0..24 {
        fx.file(dfdu, &format!("file-{i:02}.rs"), gib(0.0826 / 24.0));
    }
    for (name, size) in [
        ("brewd", 16.6),
        ("experiment", 16.3),
        ("foodchapters", 12.8),
        ("MaterialKolor", 10.9),
        ("filmstrip", 7.3),
        ("whalesay", 7.0),
        ("SIGO", 6.5),
        ("toebeans", 6.4),
        ("jolt", 4.7),
        ("compass", 3.7),
        ("bookdb", 3.5),
        ("connectivity", 3.3),
        ("kmpalette", 3.1),
        ("1se-android", 2.2),
    ] {
        let root = fx.root;
        filled(&mut fx, root, name, size);
    }

    let mut app = App::new(Theme::no_color(), t0);
    app.home = Some(PathBuf::from("/home/jo"));
    let mut tree = TreeState::default();
    for d in [dfdu, claude, worktrees, target] {
        fx.expand(&mut tree, d);
    }
    app.tree = Some(tree);
    sync(&mut app, &fx.m, t0);
    let row = row_names(&app)
        .iter()
        .position(|n| n == "target")
        .expect("target");
    app.move_to(row);
    app
}

#[test]
fn tree_at_120() {
    let t0 = Instant::now();
    insta::assert_snapshot!("tree_120", render(&mut dev(t0), t0, 120, 32));
}

#[test]
fn tree_at_80() {
    let t0 = Instant::now();
    insta::assert_snapshot!("tree_80", render(&mut dev(t0), t0, 80, 24));
}

/// The shadow of a bar is drawn dim, a category inside its match too, and fold rows are in
/// italics.
#[test]
fn tree_styles() {
    let t0 = Instant::now();
    let mut app = dev(t0);
    app.theme = Theme::named("dark", crate::theme::ColorLevel::TrueColor, None).expect("theme");
    let buf = render_buffer(&mut app, t0, 120, 32);
    let t = &app.theme;
    assert_eq!(style_at(&buf, "░").fg, t.bar_empty.fg);
    assert_eq!(style_at(&buf, "▾ ").fg, t.key.fg);
    assert_eq!(style_at(&buf, "├─").fg, t.fg_dim.fg);
    assert!(
        style_at(&buf, "… 9 more")
            .add_modifier
            .contains(Modifier::ITALIC)
    );
    assert_eq!(style_at(&buf, "tree").fg, t.key.fg);
}

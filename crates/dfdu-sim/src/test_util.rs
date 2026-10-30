//! Helpers shared by the unit tests of several modules.

use dfdu_tree::Ix;

use crate::{ROOT, Sim, SimConfig, Spec, SpecBuilder};

/// The default config without random commands, so a run only scans.
pub(crate) fn quiet() -> SimConfig {
    SimConfig {
        rescans: 0,
        deletes: 0,
        cancels: 0,
        ..SimConfig::default()
    }
}

/// /sim/{a, d/{f, g}, e/}: a file, a directory of two files and an empty directory.
pub(crate) fn small() -> Spec {
    let mut b = SpecBuilder::new();
    b.file(ROOT, "a", 100);
    let d = b.dir(ROOT, "d");
    b.file(d, "f", 5000);
    b.file(d, "g", 7000);
    b.dir(ROOT, "e");
    b.build()
}

/// A run of [`small`] with `cfg`, stepped until everything has settled.
///
/// # Panics
///
/// When a check of the run fails.
pub(crate) fn settled(cfg: &SimConfig) -> Sim {
    let mut sim = Sim::with_spec(1, cfg, small());
    if let Err(e) = sim.settle() {
        panic!("{e}");
    }
    sim
}

/// The node at `path` below the root ("" is the root), by name.
///
/// # Panics
///
/// If there is none.
pub(crate) fn find(sim: &Sim, path: &str) -> Ix {
    let t = &sim.model.tree;
    let mut ix = t.roots().next().unwrap_or_else(|| panic!("no root"));
    for name in path.split('/').filter(|n| !n.is_empty()) {
        ix = t
            .children(ix)
            .find(|&c| t.name(c) == name.as_bytes())
            .unwrap_or_else(|| panic!("no {name} in {path}"));
    }
    ix
}

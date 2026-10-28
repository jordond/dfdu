//! The tables `dfdu scan` and `dfdu import` print: the tree, disk usage by type, the entries
//! not read, and the summary on stderr.

use std::fmt::Write as _;
use std::time::Duration;

use dfdu_engine::{Model, SizeMode, Timing};
use dfdu_tree::filetype::{self, TypeFilter};
use dfdu_tree::{DirState, Ix, NodeFlags, Tree};

fn size(tree: &Tree, ix: Ix, mode: SizeMode) -> u64 {
    mode.of(&tree.total(ix))
}

/// The totals shown for `ix`: everything, or with a filter only the matching files below it.
pub fn shown_total(tree: &Tree, ix: Ix, filter: Option<&TypeFilter>) -> dfdu_tree::Totals {
    match filter {
        Some(f) => filetype::sum(tree, ix, f, None),
        None => tree.total(ix),
    }
}

/// The tree `depth` levels deep, `top` entries per directory. With a type filter, sizes count
/// the matching files only and entries without any are left out.
pub fn print_tree(
    m: &Model,
    root: Ix,
    depth: usize,
    top: usize,
    mode: SizeMode,
    filter: Option<&TypeFilter>,
) {
    let tree = &m.tree;
    let root_total = shown_total(tree, root, filter);
    if let Some(f) = filter {
        outln!("only {f}: {} files", root_total.items);
    }
    outln!(
        "{:>10}  {}{}",
        dfdu_engine::human(mode.of(&root_total)),
        String::from_utf8_lossy(tree.name(root)),
        note(m, root)
    );
    // Depth first, largest first, `top` entries per directory.
    let mut stack = vec![(root, 0usize, 0u64)];
    while let Some((ix, d, bytes)) = stack.pop() {
        if ix != root {
            let slash = if tree.dir(ix).is_some() { "/" } else { "" };
            outln!(
                "{:>10}  {}{}{}{}",
                dfdu_engine::human(bytes),
                "  ".repeat(d),
                String::from_utf8_lossy(tree.name(ix)),
                slash,
                note(m, ix)
            );
        }
        if d >= depth {
            continue;
        }
        // Data the entries share is counted at the directory.
        if mode == SizeMode::Disk && filter.is_none() && tree.home(ix) > 0 {
            outln!(
                "{:>10}  {}(shared between entries)",
                dfdu_engine::human(tree.home(ix)),
                "  ".repeat(d + 1)
            );
        }
        let mut kids: Vec<(Ix, u64)> = tree
            .children(ix)
            .filter_map(|c| match filter {
                Some(f) => {
                    let t = filetype::sum(tree, c, f, None);
                    (t.items > 0).then(|| (c, mode.of(&t)))
                }
                None => Some((c, size(tree, c, mode))),
            })
            .collect();
        kids.sort_by_key(|&(_, b)| std::cmp::Reverse(b));
        for &(c, b) in kids.iter().take(top).rev() {
            stack.push((c, d + 1, b));
        }
    }
}

/// Disk usage by file type, then the largest extensions, of the files below `root` (those
/// `filter` matches, with one).
pub fn print_by_type(tree: &Tree, root: Ix, mode: SizeMode, filter: Option<&TypeFilter>) {
    let mut b = filetype::Breakdown::default();
    let all = TypeFilter::default();
    filetype::sum(tree, root, filter.unwrap_or(&all), Some(&mut b));
    let files = |n: u64| {
        if n == 1 {
            "1 file".into()
        } else {
            format!("{n} files")
        }
    };
    outln!();
    outln!("By type:");
    let mut types: Vec<_> = filetype::FileType::ALL
        .iter()
        .map(|&t| (t, b.by_type[t as usize]))
        .filter(|(_, t)| t.items > 0)
        .collect();
    types.sort_by_key(|(_, t)| std::cmp::Reverse(mode.of(t)));
    for (ty, t) in types {
        outln!(
            "{:>10}  {:<12}{:>14}",
            dfdu_engine::human(mode.of(&t)),
            ty.name(),
            files(t.items)
        );
    }
    let exts = b.exts_by(|t| mode.of(t));
    if !exts.is_empty() {
        outln!();
        outln!("Largest extensions:");
        for (e, t) in exts.iter().take(15) {
            outln!(
                "{:>10}  {:<12}{:>14}  {}",
                dfdu_engine::human(mode.of(t)),
                format!(".{e}"),
                files(t.items),
                e.file_type()
            );
        }
    }
}

/// [`notes`] in brackets after a name, or nothing.
pub fn note(m: &Model, ix: Ix) -> String {
    let n = notes(m, ix);
    if n.is_empty() { n } else { format!("  [{n}]") }
}

/// Why an entry looks the way it does: not scanned, a second mount, a btrfs subvolume.
pub fn notes(m: &Model, ix: Ix) -> String {
    let tree = &m.tree;
    let mut parts = vec![];
    if let Some(p) = dfdu_engine::problem(tree, ix) {
        parts.push(p);
    }
    match dfdu_engine::errors_below(tree, ix) {
        0 => {}
        1 => parts.push("1 error inside".into()),
        n => parts.push(format!("{n} errors inside")),
    }
    if tree.flags(ix).has(NodeFlags::DUP_MOUNT) {
        parts.push("mounted again: counted at its other mount".to_string());
    } else if tree.flags(ix).has(NodeFlags::KERNEL) {
        parts.push("kernel filesystem, never scanned".to_string());
    } else if tree.flags(ix).has(NodeFlags::NETWORK) {
        parts.push("network filesystem, not scanned (--network)".to_string());
    } else if tree.dir(ix).is_some_and(|d| d.state == DirState::Skipped) {
        parts.push("other filesystem, not scanned (--cross-mounts)".to_string());
    }
    if let Some(l) = dfdu_engine::subvol_label(m, ix) {
        parts.push(l);
    }
    if tree.flags(ix).has(NodeFlags::COMPRESSED) {
        parts.push("compressed: at most this".into());
    }
    let out = tree.total(ix).shared_out;
    if out > 0 {
        parts.push(format!(
            "{} also used outside the scan",
            dfdu_engine::human(out)
        ));
    }
    parts.join("; ")
}

/// Every entry that could not be read, in path order.
pub fn print_errors(m: &Model, root: Ix) {
    let list = dfdu_engine::errors(&m.tree, root, usize::MAX);
    if list.items.is_empty() {
        return;
    }
    outln!();
    outln!("Not read:");
    for e in &list.items {
        outln!(
            "  {}{}: {}",
            e.path.display(),
            if e.is_dir { "/" } else { "" },
            e.problem
        );
    }
}

pub fn print_footer(m: &Model, root: Ix, elapsed: Duration, listed: bool) {
    let t = m.tree.total(root);
    let s = &m.status.stats;
    eprintln!(
        "{} items, {}, {:.2}s{}",
        t.items,
        match t.errors {
            0 => "no errors".to_string(),
            1 => "1 error".to_string(),
            n => format!("{n} errors"),
        },
        elapsed.as_secs_f64(),
        if m.status.incomplete {
            ", incomplete"
        } else {
            ""
        }
    );
    if t.errors > 0 {
        let list = dfdu_engine::errors(&m.tree, root, 0);
        let mut line = format!("not read: {}", list.summary());
        if let Some(h) = list.hint() {
            let _ = write!(line, "; {h}");
        }
        if !listed {
            line += "; --errors lists them";
        }
        eprintln!("{line}");
    }
    if t.referenced > t.alloc {
        let why = if m.status.exact_sizes {
            "shared data counted once, compressed data at its size on disk"
        } else {
            "shared data counted once"
        };
        eprintln!(
            "{} less than du ({}): {why} (--like-du for du's number)",
            dfdu_engine::human(t.referenced - t.alloc),
            dfdu_engine::human(t.referenced)
        );
    }
    if let Some((name, ratio)) = m.status.profile
        && (ratio - 1.0).abs() > f64::EPSILON
    {
        eprintln!("btrfs data profile {name}: each byte is stored {ratio:.1} times");
    }
    if m.status.compressed_upper_bound {
        eprintln!(
            "compressed files are shown at their uncompressed size, an upper bound; run as \
             root for sizes on disk"
        );
    }
    if m.status.unmeasured > 0 {
        eprintln!(
            "{} files could not be measured; data they share may be counted more than once",
            m.status.unmeasured
        );
    }
    if s.corrupt + s.lost + s.duplicate > 0 {
        eprintln!(
            "pipeline: corrupt {} lost {} duplicate {}",
            s.corrupt, s.lost, s.duplicate
        );
    }
}

/// `--debug-stats`: what went through the pipeline and how the applier spent its time (entries
/// applied per second of lock time, hold and wait times per slice).
#[expect(
    clippy::cast_precision_loss,
    reason = "counts shown as rates and MiB to one or two decimals"
)]
pub fn print_debug_stats(m: &Model) {
    let s = &m.status.stats;
    let t = &m.status.timing;
    eprintln!(
        "pipeline: {} messages, {} chunks, {} closes, {} entries; discarded {} (job) {} (cut); \
         late {}, stalled {}",
        s.messages,
        s.chunks,
        s.closes,
        s.entries,
        s.discarded_job,
        s.discarded_cut,
        s.late,
        s.stalled
    );
    let (names, garbage) = m.tree.name_bytes();
    let (kids, kids_freed) = m.tree.child_slots();
    eprintln!(
        "tree: {} nodes, {:.1} MiB of names ({:.1} MiB freed), {:.1} MiB of child lists \
         ({:.1} MiB freed)",
        m.tree.live_nodes(),
        names as f64 / 1_048_576.0,
        garbage as f64 / 1_048_576.0,
        kids as f64 * 4.0 / 1_048_576.0,
        kids_freed as f64 * 4.0 / 1_048_576.0
    );
    let locked = Duration::from_nanos(t.locked_ns);
    let rate = s.entries as f64 / locked.as_secs_f64().max(1e-9);
    let long = Timing::at_least(&t.hold, Duration::from_millis(2));
    eprintln!(
        "applier: {:.0} ms holding the write lock in {} slices, {:.2}M entries/s; hold p50 < {:?}, \
         p99 < {:?}, max < {:?}, {long} slices ({:.2}%) over 2 ms; wait for the lock p99 < {:?}, \
         max < {:?}",
        locked.as_secs_f64() * 1000.0,
        t.slices,
        rate / 1e6,
        Timing::percentile(&t.hold, 0.5),
        Timing::percentile(&t.hold, 0.99),
        Timing::max(&t.hold),
        long as f64 * 100.0 / t.slices.max(1) as f64,
        Timing::percentile(&t.wait, 0.99),
        Timing::max(&t.wait),
    );
}

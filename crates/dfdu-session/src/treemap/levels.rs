//! The Map view's settings ([`MapState`]: how many levels, what the colours show) and the levels
//! below its top-level tiles, read from the engine model ([`read`]) for the largest directories
//! only. The tiles at the top level are the rows of the current directory. At depth 1 (the
//! default) one level is still read, for what the selected tile shows. A directory without a
//! match of its own is coloured by the category most of what is matched inside it is in,
//! however deep ([`dominant`]).

use std::collections::HashMap;
use std::sync::Arc;

use dfdu_engine::{Model, SizeMode};
use dfdu_tree::filetype::{self, TypeFilter};
use dfdu_tree::{Class, Ix, NodeRef};

use crate::snapshot::{Name, Tag, class_below, tag_of};
use crate::state::SessionState;
use crate::view::View;

pub const MIN_DEPTH: u8 = 1;
pub const MAX_DEPTH: u8 = 3;
/// At most this many tiles per parent before the rest is merged.
pub const MAX_TILES: usize = 48;
/// Children the model read looks at in one read, over all directories. A directory that would
/// go over it is drawn without its contents.
const READ_BUDGET: usize = 250_000;

/// What the tile colours show; `p` steps through them in this order.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum ColorBy {
    /// The category of the rule each tile matched, or of the match it is in; a directory
    /// without one takes the category most of it is in.
    #[default]
    Category,
    /// How long ago each tile was modified.
    Age,
    /// Each top-level tile and everything inside it in its own colour.
    Folder,
}

impl ColorBy {
    pub fn label(self) -> &'static str {
        match self {
            ColorBy::Folder => "top folder",
            ColorBy::Age => "age",
            ColorBy::Category => "category",
        }
    }
}

/// The Map view's settings.
#[derive(Clone, Debug)]
pub struct MapState {
    /// Levels shown, [`MIN_DEPTH`] to [`MAX_DEPTH`].
    pub depth: u8,
    pub color: ColorBy,
}

impl Default for MapState {
    fn default() -> Self {
        MapState {
            depth: 1,
            color: ColorBy::default(),
        }
    }
}

/// What a read was for: when it changes, the levels below the top are read again.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Key {
    pub dir: Option<NodeRef>,
    pub sort_gen: u64,
    pub depth: u8,
    pub mode: SizeMode,
    pub hide_hidden: bool,
    /// Hash of the type filter.
    pub types: u64,
}

/// What a read of the levels below is for, as the browser stands.
pub fn key_of(s: &SessionState) -> Key {
    Key {
        dir: s.snap.dir_node,
        sort_gen: s.list.sort_gen,
        depth: s.map.depth,
        mode: s.size_mode,
        hide_hidden: s.hide_hidden,
        types: {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            s.types.hash(&mut h);
            h.finish()
        },
    }
}

/// The levels below the top-level tiles, as the last read found them.
#[derive(Clone, Debug, Default)]
pub struct Info {
    pub key: Option<Key>,
    /// Shared, so that the copy each snapshot read makes is cheap.
    pub below: Arc<HashMap<NodeRef, Kids>>,
    /// The category colour slot of most of what is matched below each directory under the
    /// current one (`dominant`).
    pub dominant: Arc<HashMap<NodeRef, usize>>,
    /// How many nodes the tree had classified: matches found or decided after the read (a scan
    /// classifies as it goes) make it read again.
    pub classes: usize,
}

/// The largest children of a directory, and how many others there are and their total size.
#[derive(Clone, Debug, Default)]
pub struct Kids {
    pub items: Vec<Kid>,
    pub rest: (u64, u64),
}

#[derive(Clone, Debug)]
pub struct Kid {
    pub node: NodeRef,
    pub name: Name,
    pub is_dir: bool,
    pub size: u64,
    pub mtime: i64,
    pub tag: Option<Tag>,
    pub kids: Option<Kids>,
}

/// The view is the treemap, also while the front end has no room for it.
pub fn on(s: &SessionState) -> bool {
    s.current_view() == View::Map
}

/// True when the treemap needs the levels below the top read again.
pub fn stale(s: &SessionState) -> bool {
    on(s) && s.extras.treemap.key != Some(key_of(s))
}

/// Reads the levels below the largest top-level directories. Runs under the read lock and
/// visits at most `READ_BUDGET` children; nothing when the view is off or the last read is
/// still current.
pub fn read(m: &Model, s: &SessionState) -> Info {
    if !on(s) {
        return Info::default();
    }
    let key = key_of(s);
    let classes = m.tree.classes().count();
    let last = &s.extras.treemap;
    if last.key == Some(key) && last.classes == classes {
        return last.clone();
    }
    let mut below = HashMap::new();
    // One level even at depth 1: what the selected tile shows.
    let levels = s.map.depth.max(2) - 1;
    let dir = s.snap.dir_node.and_then(|n| m.tree.resolve_visible(n));
    let info = |below| Info {
        key: Some(key),
        below: Arc::new(below),
        dominant: Arc::new(dir.map_or_else(HashMap::new, |d| dominant(m, d, key.mode))),
        classes,
    };
    let entries = &s.snap.entries;
    let mut dirs: Vec<(u64, usize)> = (s.list.rows.iter())
        .filter(|&&i| entries[i].is_dir && !entries[i].pseudo)
        .map(|&i| (entries[i].size, i))
        .collect();
    largest_first(&mut dirs, MAX_TILES);
    let mut budget = READ_BUDGET;
    for (_, i) in dirs {
        let node = entries[i].node;
        if let Some(ix) = m.tree.resolve_visible(node)
            && let Some(kids) = read_kids(
                m,
                ix,
                m.rules
                    .as_ref()
                    .and_then(|r| r.class_in_effect(&m.tree, ix)),
                levels,
                &key,
                &s.types,
                &mut budget,
            )
        {
            below.insert(node, kids);
        }
    }
    info(below)
}

/// The category colour slot that holds the most matched bytes below each directory under
/// `dir`, from the tree's matches: a directory without a match of its own takes the category of
/// most of what is matched inside it, however deep (a project is its `node_modules/`). A match
/// inside another counts as part of the outer one. One walk up from each match.
fn dominant(m: &Model, dir: Ix, mode: SizeMode) -> HashMap<NodeRef, usize> {
    let Some(rules) = m.rules.as_deref() else {
        return HashMap::new();
    };
    let t = &m.tree;
    let mut by: HashMap<u32, HashMap<usize, u64>> = HashMap::new();
    'matches: for (c, class) in t.classes() {
        let mut ups = Vec::new();
        let mut at = t.parent(c);
        loop {
            match at {
                // Not below `dir`.
                None => continue 'matches,
                Some(a) if a == dir => break,
                // Inside another match, which holds it.
                Some(a) if t.class(a).is_some() => continue 'matches,
                Some(a) => {
                    ups.push(a);
                    at = t.parent(a);
                }
            }
        }
        let slot = rules.rule(class.rule).category().color as usize;
        let size = mode.of(&t.total(c));
        for a in ups {
            *by.entry(a.0).or_default().entry(slot).or_default() += size;
        }
    }
    by.into_iter()
        .filter_map(|(a, slots)| {
            let best = slots
                .into_iter()
                .max_by_key(|&(slot, size)| (size, std::cmp::Reverse(slot)))?;
            Some((t.node_ref(Ix(a)), best.0))
        })
        .collect()
}

/// Keeps the `n` largest of `v`, largest first.
fn largest_first<T: Ord>(v: &mut Vec<(u64, T)>, n: usize) {
    let by_size = |a: &(u64, T), b: &(u64, T)| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1));
    if v.len() > n {
        v.select_nth_unstable_by(n, by_size);
        v.truncate(n);
    }
    v.sort_unstable_by(by_size);
}

/// The children of `dir`, `levels` deep; `inside` is the class in effect for them. With a type
/// filter, sizes count the matching files only and children without any are left out.
fn read_kids(
    m: &Model,
    dir: Ix,
    inside: Option<(Ix, Class)>,
    levels: u8,
    key: &Key,
    types: &TypeFilter,
    budget: &mut usize,
) -> Option<Kids> {
    let tree = &m.tree;
    let children = tree.children(dir);
    if levels == 0 || children.len() > *budget {
        return None;
    }
    *budget -= children.len();
    let mut all: Vec<(u64, u32)> = children
        .filter(|&c| !(key.hide_hidden && tree.name(c).first() == Some(&b'.')))
        .filter_map(|c| {
            if types.is_empty() {
                return Some((key.mode.of(&tree.total(c)), c.0));
            }
            let t = filetype::sum(tree, c, types, None);
            (t.items > 0).then(|| (key.mode.of(&t), c.0))
        })
        .collect();
    let total: u64 = all.iter().map(|a| a.0).sum();
    let listed = all.len() as u64;
    largest_first(&mut all, MAX_TILES);
    let shown: u64 = all.iter().map(|a| a.0).sum();
    let rest_n = if types.is_empty() {
        tree.children(dir).len() as u64 - all.len() as u64
    } else {
        listed - all.len() as u64
    };
    let items = all
        .into_iter()
        .map(|(size, c)| {
            let c = Ix(c);
            let is_dir = tree.dir(c).is_some();
            Kid {
                node: tree.node_ref(c),
                name: tree.name(c).into(),
                is_dir,
                size,
                mtime: tree.mtime(c),
                tag: tag_of(m, inside, c),
                kids: if is_dir {
                    let below = class_below(m, inside, c);
                    read_kids(m, c, below, levels - 1, key, types, budget)
                } else {
                    None
                },
            }
        })
        .collect();
    Some(Kids {
        items,
        rest: (rest_n, total - shown),
    })
}

/// One level more or less, within [`MIN_DEPTH`] and [`MAX_DEPTH`].
pub fn deeper(s: &mut SessionState, by: i8) {
    let t = &mut s.map;
    t.depth = t
        .depth
        .saturating_add_signed(by)
        .clamp(MIN_DEPTH, MAX_DEPTH);
    s.status = Some(format!("map depth {}", t.depth));
}

/// The colouring drawn: by category needs the rules, and without them is by top folder.
pub fn color_shown(s: &SessionState) -> ColorBy {
    match s.map.color {
        ColorBy::Category if s.snap.rules.is_none() => ColorBy::Folder,
        c => c,
    }
}

/// `p`: the next colouring, category, age, then top folder (category only with rules).
pub fn cycle_color(s: &mut SessionState) {
    let next = match color_shown(s) {
        ColorBy::Category => ColorBy::Age,
        ColorBy::Age => ColorBy::Folder,
        ColorBy::Folder if s.snap.rules.is_some() => ColorBy::Category,
        ColorBy::Folder => ColorBy::Age,
    };
    s.map.color = next;
    s.status = Some(format!("map colour: {}", next.label()));
}

#[cfg(test)]
mod tests {
    use dfdu_engine::SizeMode;

    use super::*;
    use crate::test_support::Fixture;

    #[test]
    fn read_stops_at_the_budget() {
        let (mut fx, root) = Fixture::new();
        let d = fx.dir(root, "d");
        fx.file(d, "x", 10);
        let mut budget = 0;
        let key = Key {
            dir: None,
            sort_gen: 0,
            depth: 2,
            mode: SizeMode::Disk,
            hide_hidden: false,
            types: 0,
        };
        let all = TypeFilter::default();
        assert!(read_kids(&fx.m, d, None, 1, &key, &all, &mut budget).is_none());
        budget = 1;
        assert!(read_kids(&fx.m, d, None, 1, &key, &all, &mut budget).is_some());
        assert_eq!(budget, 0);
    }

    #[test]
    fn depth_stays_in_bounds_and_colours_need_the_rules() {
        let mut s = SessionState::new(std::time::Instant::now());
        deeper(&mut s, -1);
        assert_eq!(s.map.depth, MIN_DEPTH);
        for _ in 0..5 {
            deeper(&mut s, 1);
        }
        assert_eq!(s.map.depth, MAX_DEPTH);
        assert_eq!(s.status.as_deref(), Some("map depth 3"));
        // Without rules there is no colouring by category to step to.
        assert_eq!(color_shown(&s), ColorBy::Folder);
        cycle_color(&mut s);
        assert_eq!(s.map.color, ColorBy::Age);
        cycle_color(&mut s);
        cycle_color(&mut s);
        assert_eq!(s.map.color, ColorBy::Age, "past top folder to age");
    }
}

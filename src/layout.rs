//! Pure geometry: squarified treemap layout, independent of rendering.
//!
//! ## Nesting modes
//!
//! * [`Nesting::Flat`] (default): only files get rectangles; directories are
//!   implicit, and every file's area is exactly proportional to its size.
//! * [`Nesting::Header`]: **one rectangle per directory and per file**. A
//!   directory's rectangle starts with a one-row title bar, and its children
//!   are laid out recursively in the rest.
//!
//! ## Proportionality
//!
//! Every rectangle's area is proportional to its size **relative to its
//! siblings**: they split their parent's interior by size, exactly, before
//! snapping to cells.
//!
//! `Flat` mode has no chrome, so every file's area is exactly
//! `size / total * view_area` across the whole view. In `Header` mode, each
//! directory's title row takes cells from its contents, so files in different
//! directories are *not* exactly comparable. A deeply nested file looks a bit
//! smaller than a top-level file of the same size.
//!
//! ## Snapping
//!
//! Rectangles are computed in continuous space and snapped to terminal cells
//! by rounding each edge. Siblings share bit-identical edge coordinates, so
//! snapped tiles cover their parent's interior with no gaps or overlaps. Each
//! edge moves by less than half a cell. Anything that snaps to nothing is not
//! drawn and is counted in [`HiddenStats`]. A directory too small for its
//! chrome is drawn as a single solid block ([`TileKind::CollapsedDir`]).
//!
//! Terminal cells are about twice as tall as they are wide, so squarify runs
//! in "physical" units (a cell is `1 × cell_aspect`) to make tiles look square
//! on screen. That changes only their shapes, never their areas.

use ratatui::layout::Rect;

use crate::tree::{DiskTree, NodeId};

/// Axis-aligned rectangle stored as edges, so neighbours can share an edge
/// value exactly.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RectF {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl RectF {
    pub fn new(x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
        RectF { x0, y0, x1, y1 }
    }
    pub fn width(&self) -> f64 {
        self.x1 - self.x0
    }
    pub fn height(&self) -> f64 {
        self.y1 - self.y0
    }
    pub fn area(&self) -> f64 {
        self.width() * self.height()
    }
}

/// Squarified treemap (Bruls, Huizing & van Wijk, 2000).
///
/// `weights` must be non-negative and sorted in descending order. Returns one
/// rectangle per weight, in the same order, whose areas are proportional to
/// the weights and which exactly tile `rect`.
pub fn squarify(weights: &[f64], rect: RectF) -> Vec<RectF> {
    let mut out = Vec::with_capacity(weights.len());
    let mut remaining: f64 = weights.iter().sum();
    if weights.is_empty() || remaining <= 0.0 {
        out.resize(
            weights.len(),
            RectF::new(rect.x0, rect.y0, rect.x0, rect.y0),
        );
        return out;
    }
    let mut rest = rect;
    let mut i = 0;
    while i < weights.len() {
        let (w, h) = (rest.width(), rest.height());
        let short = w.min(h);
        let scale = if remaining > 0.0 {
            w * h / remaining
        } else {
            0.0
        };

        // Grow the row while it improves the worst aspect ratio.
        let mut j = i + 1;
        let mut row_sum = weights[i];
        let mut best = worst_ratio(weights[i], weights[i], row_sum, short, scale);
        while j < weights.len() {
            let candidate = worst_ratio(weights[i], weights[j], row_sum + weights[j], short, scale);
            if candidate > best {
                break;
            }
            best = candidate;
            row_sum += weights[j];
            j += 1;
        }

        let last_row = j == weights.len();
        let frac = if remaining > 0.0 {
            row_sum / remaining
        } else {
            1.0
        };
        let row = &weights[i..j];
        if w >= h {
            // Column along the left edge, items stacked top to bottom.
            let split = if last_row {
                rest.x1
            } else {
                rest.x0 + w * frac
            };
            stack(row, row_sum, rest.y0, rest.y1, |a, b| {
                out.push(RectF::new(rest.x0, a, split, b))
            });
            rest.x0 = split;
        } else {
            // Row along the top edge, items left to right.
            let split = if last_row {
                rest.y1
            } else {
                rest.y0 + h * frac
            };
            stack(row, row_sum, rest.x0, rest.x1, |a, b| {
                out.push(RectF::new(a, rest.y0, b, split))
            });
            rest.y0 = split;
        }
        remaining -= row_sum;
        i = j;
    }
    out
}

/// Split `[start, end]` into segments proportional to `row`, using
/// cumulative sums so the final edge lands exactly on `end`.
fn stack(row: &[f64], row_sum: f64, start: f64, end: f64, mut emit: impl FnMut(f64, f64)) {
    let len = end - start;
    let mut acc = 0.0;
    let mut prev = start;
    for (k, &wt) in row.iter().enumerate() {
        acc += wt;
        let next = if k + 1 == row.len() || row_sum <= 0.0 {
            end
        } else {
            start + len * (acc / row_sum)
        };
        emit(prev, next);
        prev = next;
    }
}

/// Worst aspect ratio of a row with largest weight `max`, smallest `min`,
/// total `sum`, laid along a side of length `short`.
fn worst_ratio(max: f64, min: f64, sum: f64, short: f64, scale: f64) -> f64 {
    let s = sum * scale;
    let (rmax, rmin) = (max * scale, min * scale);
    if s <= 0.0 || rmin <= 0.0 || short <= 0.0 {
        return f64::INFINITY;
    }
    let s2 = s * s;
    let w2 = short * short;
    (w2 * rmax / s2).max(s2 / (w2 * rmin))
}

/// How directories are represented.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Nesting {
    /// No directory rectangles; files tile the view with globally exact areas.
    #[default]
    Flat,
    /// Each directory is a rectangle with a one-row title bar; children fill
    /// the rest, recursively.
    Header,
}

impl Nesting {
    /// Cells taken from a directory rectangle: (left, top, right, bottom).
    fn insets(self) -> (u16, u16, u16, u16) {
        match self {
            Nesting::Header => (0, 1, 0, 0),
            Nesting::Flat => (0, 0, 0, 0),
        }
    }

    /// Smallest directory rectangle (width, height) that gets chrome. Anything
    /// smaller is drawn as a labelled [`TileKind::CollapsedDir`] block, since a
    /// title bar with no room for text or contents is just noise.
    fn min_dir_size(self) -> (u16, u16) {
        match self {
            Nesting::Header => (3, 2),
            Nesting::Flat => (0, 0),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct LayoutOptions {
    /// Height of a terminal cell divided by its width. ~2.0 for most fonts.
    pub cell_aspect: f64,
    pub nesting: Nesting,
}

impl Default for LayoutOptions {
    fn default() -> Self {
        LayoutOptions {
            cell_aspect: 2.0,
            nesting: Nesting::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TileKind {
    /// A file (or symlink, socket, ...).
    File,
    /// A directory drawn with its chrome; its children are tiles inside
    /// [`Tile::inner`].
    Dir,
    /// A directory too small for its chrome, drawn as one solid block.
    CollapsedDir,
}

/// One visible rectangle.
#[derive(Clone, Debug)]
pub struct Tile {
    pub node: NodeId,
    /// The node's parent. The view root is never a tile itself.
    pub parent: NodeId,
    pub kind: TileKind,
    /// Snapped rectangle in terminal cells (absolute buffer coordinates).
    pub rect: Rect,
    /// For [`TileKind::Dir`]: the interior holding the children (`rect` minus
    /// chrome). Otherwise equal to `rect`.
    pub inner: Rect,
    /// Exact, unsnapped rectangle in fractional cells.
    pub exact: RectF,
    /// Depth below the view root (direct children are depth 1).
    pub depth: u16,
    /// Index (by size rank) of the view root's child containing this node.
    /// Handy for colouring each top-level subtree as a family.
    pub group: usize,
    /// Rank of this node among its siblings (0 = largest).
    pub sibling_rank: usize,
}

/// Files not individually drawn: smaller than a cell, or inside a
/// [`TileKind::CollapsedDir`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HiddenStats {
    pub files: u64,
    pub bytes: u64,
}

/// Result of laying out one view (a subtree root in a screen area).
#[derive(Clone, Debug)]
pub struct TreemapLayout {
    pub area: Rect,
    pub root: NodeId,
    pub nesting: Nesting,
    /// Pre-order: every directory tile comes before the tiles inside it, and
    /// siblings are in size order.
    pub tiles: Vec<Tile>,
    pub hidden: HiddenStats,
    /// For O(1) hit testing: index into `tiles` of the *innermost* tile at each
    /// cell, row-major.
    cell_owner: Vec<u32>,
}

const NO_TILE: u32 = u32::MAX;

impl TreemapLayout {
    pub fn compute(tree: &DiskTree, root: NodeId, area: Rect, opts: &LayoutOptions) -> Self {
        let mut ctx = Ctx {
            tree,
            aspect: opts.cell_aspect.max(0.1),
            nesting: opts.nesting,
            tiles: Vec::new(),
            hidden: HiddenStats::default(),
        };
        let root_node = tree.node(root);
        if root_node.size == 0 || area.is_empty() {
            ctx.hidden.files = root_node.file_count;
        } else if !root_node.is_dir() {
            // Viewing a single file: it fills the view.
            let parent = tree.parent(root).unwrap_or(root);
            let exact = RectF::new(
                area.x as f64,
                area.y as f64,
                area.right() as f64,
                area.bottom() as f64,
            );
            ctx.push(root, parent, TileKind::File, area, area, exact, 0, 0, 0);
        } else {
            // The view root is not drawn; its children fill the area.
            ctx.children(root, ctx.phys(area), 1, None);
        }

        // Tiles are pre-order, so painting in order leaves the innermost owner.
        let mut cell_owner = vec![NO_TILE; area.width as usize * area.height as usize];
        for (i, t) in ctx.tiles.iter().enumerate() {
            for y in t.rect.top()..t.rect.bottom() {
                let row = (y - area.y) as usize * area.width as usize;
                for x in t.rect.left()..t.rect.right() {
                    cell_owner[row + (x - area.x) as usize] = i as u32;
                }
            }
        }
        TreemapLayout {
            area,
            root,
            nesting: opts.nesting,
            tiles: ctx.tiles,
            hidden: ctx.hidden,
            cell_owner,
        }
    }

    /// Innermost tile covering the absolute cell `(x, y)`: a file, a collapsed
    /// directory, or a directory's chrome.
    pub fn tile_at(&self, x: u16, y: u16) -> Option<&Tile> {
        let a = self.area;
        if x < a.left() || x >= a.right() || y < a.top() || y >= a.bottom() {
            return None;
        }
        let i = self.cell_owner[(y - a.y) as usize * a.width as usize + (x - a.x) as usize];
        self.tiles.get(i as usize)
    }

    pub fn tile_of(&self, node: NodeId) -> Option<&Tile> {
        self.tiles.iter().find(|t| t.node == node)
    }

    /// Tiles whose parent is `parent`, largest first.
    pub fn children_of(&self, parent: NodeId) -> impl Iterator<Item = &Tile> {
        self.tiles.iter().filter(move |t| t.parent == parent)
    }
}

struct Ctx<'a> {
    tree: &'a DiskTree,
    aspect: f64,
    nesting: Nesting,
    tiles: Vec<Tile>,
    hidden: HiddenStats,
}

impl Ctx<'_> {
    /// Absolute cell rect -> physical-space rect.
    fn phys(&self, r: Rect) -> RectF {
        RectF::new(
            r.x as f64,
            r.y as f64 * self.aspect,
            r.right() as f64,
            r.bottom() as f64 * self.aspect,
        )
    }

    /// Physical rect -> (snapped cell rect, exact rect in cells). `None` if it
    /// collapses to nothing.
    fn snap(&self, r: RectF) -> Option<(Rect, RectF)> {
        let exact = RectF::new(r.x0, r.y0 / self.aspect, r.x1, r.y1 / self.aspect);
        let (x0, x1) = (exact.x0.round(), exact.x1.round());
        let (y0, y1) = (exact.y0.round(), exact.y1.round());
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        let rect = Rect::new(x0 as u16, y0 as u16, (x1 - x0) as u16, (y1 - y0) as u16);
        Some((rect, exact))
    }

    fn hide(&mut self, id: NodeId) {
        let n = self.tree.node(id);
        self.hidden.files += n.file_count;
        self.hidden.bytes += n.size;
    }

    #[allow(clippy::too_many_arguments)]
    fn push(
        &mut self,
        node: NodeId,
        parent: NodeId,
        kind: TileKind,
        rect: Rect,
        inner: Rect,
        exact: RectF,
        depth: u16,
        group: usize,
        sibling_rank: usize,
    ) {
        self.tiles.push(Tile {
            node,
            parent,
            kind,
            rect,
            inner,
            exact,
            depth,
            group,
            sibling_rank,
        });
    }

    /// Lay out `dir`'s children in the physical rect `r`. `group` is `None`
    /// at the top level, where each child starts its own group.
    fn children(&mut self, dir: NodeId, r: RectF, depth: u16, group: Option<usize>) {
        let kids: Vec<NodeId> = self
            .tree
            .node(dir)
            .children
            .iter()
            .copied()
            .filter(|&c| {
                let n = self.tree.node(c);
                if n.size == 0 {
                    // Zero bytes means zero area: nothing to draw.
                    self.hidden.files += n.file_count;
                }
                n.size > 0
            })
            .collect();
        let weights: Vec<f64> = kids
            .iter()
            .map(|&c| self.tree.node(c).size as f64)
            .collect();
        for (rank, (&c, cr)) in kids.iter().zip(squarify(&weights, r)).enumerate() {
            self.place(c, dir, cr, depth, group.unwrap_or(rank), rank);
        }
    }

    fn place(
        &mut self,
        id: NodeId,
        parent: NodeId,
        r: RectF,
        depth: u16,
        group: usize,
        rank: usize,
    ) {
        // Rounding is monotone, so if this rect snaps to nothing, so does
        // everything inside it: prune the whole subtree.
        let Some((rect, exact)) = self.snap(r) else {
            self.hide(id);
            return;
        };
        let node = self.tree.node(id);
        if !node.is_dir() {
            self.push(
                id,
                parent,
                TileKind::File,
                rect,
                rect,
                exact,
                depth,
                group,
                rank,
            );
            return;
        }
        if self.nesting == Nesting::Flat {
            // No chrome: children share the *unsnapped* rect, which keeps
            // every file's area globally exact.
            self.children(id, r, depth + 1, Some(group));
            return;
        }
        let (l, t, rt, b) = self.nesting.insets();
        let inner = Rect::new(
            rect.x + l,
            rect.y + t,
            rect.width.saturating_sub(l + rt),
            rect.height.saturating_sub(t + b),
        );
        let (min_w, min_h) = self.nesting.min_dir_size();
        if inner.is_empty() || rect.width < min_w || rect.height < min_h {
            self.push(
                id,
                parent,
                TileKind::CollapsedDir,
                rect,
                rect,
                exact,
                depth,
                group,
                rank,
            );
            self.hide(id);
            return;
        }
        self.push(
            id,
            parent,
            TileKind::Dir,
            rect,
            inner,
            exact,
            depth,
            group,
            rank,
        );
        // Children split the snapped interior, so they tile it exactly.
        self.children(id, self.phys(inner), depth + 1, Some(group));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::DiskTreeBuilder;

    fn sample() -> DiskTree {
        let mut b = DiskTreeBuilder::new("/r");
        let r = b.root();
        let a = b.add_dir(r, "a");
        b.add_file(a, "a1", 600);
        b.add_file(a, "a2", 300);
        let aa = b.add_dir(a, "aa");
        b.add_file(aa, "aa1", 250);
        b.add_file(aa, "aa2", 50);
        b.add_file(r, "big", 1200);
        b.add_file(r, "mid", 400);
        b.add_file(r, "small", 200);
        b.add_file(r, "empty", 0);
        b.finish()
    }

    fn opts(nesting: Nesting) -> LayoutOptions {
        LayoutOptions {
            nesting,
            ..Default::default()
        }
    }

    fn cells(r: Rect) -> f64 {
        r.width as f64 * r.height as f64
    }

    /// Every cell of `area` is covered by exactly one of `rects`.
    fn assert_tiles(area: Rect, rects: &[Rect]) {
        let mut hits = vec![0u8; (area.width * area.height) as usize];
        for r in rects {
            assert!(
                area.contains(r.as_position())
                    && r.right() <= area.right()
                    && r.bottom() <= area.bottom()
            );
            for y in r.top()..r.bottom() {
                for x in r.left()..r.right() {
                    hits[((y - area.y) * area.width + (x - area.x)) as usize] += 1;
                }
            }
        }
        assert!(
            hits.iter().all(|&h| h == 1),
            "gap or overlap in {area:?}: {hits:?}"
        );
    }

    #[test]
    fn squarify_tiles_rect_proportionally() {
        let w = [6.0, 6.0, 4.0, 3.0, 2.0, 2.0, 1.0];
        let rs = squarify(&w, RectF::new(0.0, 0.0, 6.0, 4.0));
        let total: f64 = rs.iter().map(RectF::area).sum();
        assert!((total - 24.0).abs() < 1e-9);
        for (wt, r) in w.iter().zip(&rs) {
            assert!((r.area() - wt).abs() < 1e-9, "{wt} vs {r:?}");
        }
    }

    #[test]
    fn header_has_one_tile_per_dir_and_file() {
        let t = sample();
        let area = Rect::new(2, 1, 80, 30);
        let l = TreemapLayout::compute(&t, t.root(), area, &opts(Nesting::Header));
        let names: Vec<(&str, TileKind)> = l
            .tiles
            .iter()
            .map(|x| (t.node(x.node).name.as_str(), x.kind))
            .collect();
        // Pre-order, siblings largest first (a = 1200 ties big = 1200; "a" < "big").
        assert_eq!(
            names,
            vec![
                ("a", TileKind::Dir),
                ("a1", TileKind::File),
                ("a2", TileKind::File),
                ("aa", TileKind::Dir),
                ("aa1", TileKind::File),
                ("aa2", TileKind::File),
                ("big", TileKind::File),
                ("mid", TileKind::File),
                ("small", TileKind::File),
            ]
        );
    }

    #[test]
    fn header_children_tile_their_parents_interior() {
        let t = sample();
        let area = Rect::new(2, 1, 80, 30);
        let l = TreemapLayout::compute(&t, t.root(), area, &opts(Nesting::Header));
        let top: Vec<Rect> = l.children_of(t.root()).map(|x| x.rect).collect();
        assert_tiles(area, &top);
        for dir in l.tiles.iter().filter(|x| x.kind == TileKind::Dir) {
            // Exactly one title row is spent.
            let r = dir.rect;
            assert_eq!(dir.inner, Rect::new(r.x, r.y + 1, r.width, r.height - 1));
            let kids: Vec<Rect> = l.children_of(dir.node).map(|x| x.rect).collect();
            assert_tiles(dir.inner, &kids);
        }
    }

    #[test]
    fn siblings_are_exactly_proportional() {
        let t = sample();
        let area = Rect::new(0, 0, 80, 30);
        for nesting in [Nesting::Header, Nesting::Flat] {
            let l = TreemapLayout::compute(&t, t.root(), area, &opts(nesting));
            for parent in l.tiles.iter().map(|x| x.parent) {
                let sibs: Vec<&Tile> = l.children_of(parent).collect();
                let area_sum: f64 = sibs.iter().map(|x| x.exact.area()).sum();
                let size_sum: f64 = sibs.iter().map(|x| t.node(x.node).size as f64).sum();
                for s in &sibs {
                    let want = t.node(s.node).size as f64 / size_sum * area_sum;
                    assert!((s.exact.area() - want).abs() < 1e-6, "{nesting:?}");
                    // Snapping moves each edge by at most half a cell.
                    let slack = s.exact.width() + s.exact.height() + 1.0;
                    assert!((cells(s.rect) - want).abs() <= slack);
                }
            }
        }
    }

    #[test]
    fn flat_areas_are_globally_exact() {
        let t = sample();
        let area = Rect::new(3, 2, 80, 24);
        let l = TreemapLayout::compute(&t, t.root(), area, &opts(Nesting::Flat));
        let total = t.node(t.root()).size as f64;
        assert!(l.tiles.iter().all(|x| x.kind == TileKind::File));
        assert_eq!(l.tiles.len(), 7);
        for tile in &l.tiles {
            let want = t.node(tile.node).size as f64 / total * cells(area);
            assert!((tile.exact.area() - want).abs() < 1e-6);
        }
        let rects: Vec<Rect> = l.tiles.iter().map(|x| x.rect).collect();
        assert_tiles(area, &rects);
        assert_eq!(l.hidden, HiddenStats { files: 1, bytes: 0 }); // "empty"
    }

    #[test]
    fn small_dirs_collapse_and_hide_contents() {
        let mut b = DiskTreeBuilder::new("/r");
        let r = b.root();
        b.add_file(r, "big", 900);
        let d = b.add_dir(r, "d");
        b.add_file(d, "f1", 60);
        b.add_file(d, "f2", 40);
        let t = b.finish();
        // "d" gets 10% of 20x5: a 2x5 column, too narrow for a title bar.
        let area = Rect::new(0, 0, 20, 5);
        let l = TreemapLayout::compute(&t, t.root(), area, &opts(Nesting::Header));
        let tile = l.tile_of(d).expect("d is drawn");
        assert_eq!(tile.kind, TileKind::CollapsedDir);
        assert_eq!(tile.rect, Rect::new(18, 0, 2, 5));
        assert_eq!(
            l.hidden,
            HiddenStats {
                files: 2,
                bytes: 100
            }
        );
        assert_eq!(l.tiles.len(), 2);
    }

    #[test]
    fn flat_is_the_default() {
        assert_eq!(LayoutOptions::default().nesting, Nesting::Flat);
    }

    #[test]
    fn tiny_files_are_reported_hidden() {
        let mut b = DiskTreeBuilder::new("/r");
        let r = b.root();
        b.add_file(r, "huge", 1_000_000);
        b.add_file(r, "speck", 1);
        let t = b.finish();
        let l = TreemapLayout::compute(&t, t.root(), Rect::new(0, 0, 10, 5), &Default::default());
        assert_eq!(l.tiles.len(), 1);
        assert_eq!(l.hidden, HiddenStats { files: 1, bytes: 1 });
        assert_eq!(l.tile_at(9, 4).map(|t| t.node), Some(l.tiles[0].node));
    }

    #[test]
    fn hit_test_finds_innermost_tile() {
        let t = sample();
        let area = Rect::new(0, 0, 80, 30);
        let l = TreemapLayout::compute(&t, t.root(), area, &opts(Nesting::Header));
        let a = l.tiles.iter().find(|x| t.node(x.node).name == "a").unwrap();
        // Title-bar cell -> the directory itself.
        assert_eq!(l.tile_at(a.rect.x, a.rect.y).unwrap().node, a.node);
        // Interior cell -> one of its descendants.
        let inside = l.tile_at(a.inner.x, a.inner.y).unwrap();
        assert!(t.is_ancestor_or_self(a.node, inside.node) && inside.node != a.node);
    }
}

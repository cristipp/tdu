//! The ratatui widget ([`TreeMap`]) and its interactive state ([`TreeMapState`]).

use std::collections::HashSet;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, StatefulWidget, Widget};

use crate::color::{ColorMode, Hsl};
use crate::layout::{LayoutOptions, Nesting, Tile, TileKind, TreemapLayout};
use crate::tree::{DiskTree, NodeId, format_size};

/// How tiles are coloured.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ColorBy {
    /// One hue per child of the view root; each top-level subtree reads as a
    /// colour family.
    #[default]
    TopLevel,
    /// Hue from the file extension (all `.rs` files look alike).
    Extension,
    /// Hue from nesting depth.
    Depth,
}

/// What gets written inside tiles that are large enough.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Labels {
    None,
    Name,
    /// Name on the first row, size on the second.
    #[default]
    NameAndSize,
}

/// Custom colouring hook: return the tile's background colour.
pub type ColorFn<'a> = Box<dyn Fn(&DiskTree, &Tile) -> Color + 'a>;

/// Treemap of a [`DiskTree`]. By default ([`Nesting::Flat`]), every file is a
/// rectangle whose area is exactly proportional to its size. With
/// [`Nesting::Header`], every directory also gets its own rectangle, a title bar
/// on top of its contents, nested recursively.
/// Render with a [`TreeMapState`], which owns the view root, the selection and
/// the cached layout.
///
/// ```no_run
/// # use tdu::*;
/// # fn f(frame: &mut ratatui::Frame, tree: &DiskTree, state: &mut TreeMapState) {
/// let map = TreeMap::new(tree)
///     .nesting(Nesting::Header)
///     .color_by(ColorBy::Extension);
/// frame.render_stateful_widget(map, frame.area(), state);
/// # }
/// ```
pub struct TreeMap<'a> {
    tree: &'a DiskTree,
    block: Option<Block<'a>>,
    color_by: ColorBy,
    color_fn: Option<ColorFn<'a>>,
    labels: Labels,
    layout: LayoutOptions,
    highlight_dir: bool,
    selected_style: Style,
    color_mode: ColorMode,
}

impl<'a> TreeMap<'a> {
    pub fn new(tree: &'a DiskTree) -> Self {
        TreeMap {
            tree,
            block: None,
            color_by: ColorBy::default(),
            color_fn: None,
            labels: Labels::default(),
            layout: LayoutOptions::default(),
            highlight_dir: true,
            selected_style: Style::default()
                .bg(Color::Rgb(250, 250, 250))
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
            color_mode: ColorMode::default(),
        }
    }

    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = Some(block);
        self
    }

    /// How directories are drawn (default [`Nesting::Flat`]).
    pub fn nesting(mut self, nesting: Nesting) -> Self {
        self.layout.nesting = nesting;
        self
    }

    pub fn color_by(mut self, c: ColorBy) -> Self {
        self.color_by = c;
        self
    }

    /// Overrides [`ColorBy`] for file tiles.
    pub fn color_fn(mut self, f: impl Fn(&DiskTree, &Tile) -> Color + 'a) -> Self {
        self.color_fn = Some(Box::new(f));
        self
    }

    /// Colour depth of the target terminal (default [`ColorMode::TrueColor`]).
    /// Use [`ColorMode::detect`] to pick from the environment.
    pub fn color_mode(mut self, mode: ColorMode) -> Self {
        self.color_mode = mode;
        self
    }

    pub fn labels(mut self, l: Labels) -> Self {
        self.labels = l;
        self
    }

    /// Cell height / width of the terminal font (default 2.0). Only affects
    /// tile shapes, never their areas.
    pub fn cell_aspect(mut self, aspect: f64) -> Self {
        self.layout.cell_aspect = aspect;
        self
    }

    /// Emphasise the selection's enclosing directories (default on). Flat mode
    /// brightens the files of the selected file's directory; header mode
    /// brightens the title bars of every enclosing directory.
    pub fn highlight_dir(mut self, on: bool) -> Self {
        self.highlight_dir = on;
        self
    }

    pub fn selected_style(mut self, s: Style) -> Self {
        self.selected_style = s;
        self
    }

    fn hue(&self, tile: &Tile) -> Option<f64> {
        let node = self.tree.node(tile.node);
        Some(match self.color_by {
            ColorBy::TopLevel => tile.group as f64 * 137.508, // golden angle
            ColorBy::Depth => tile.depth as f64 * 67.0 + 200.0,
            ColorBy::Extension if node.is_dir() => return None, // neutral frames
            ColorBy::Extension => {
                let ext = node.name.rsplit_once('.').map_or("", |(_, e)| e);
                (fnv1a(ext.as_bytes()) % 360) as f64
            }
        })
    }

    fn file_color(&self, tile: &Tile) -> Hsl {
        let node = self.tree.node(tile.node);
        // Alternate lightness among siblings and add a little per-name jitter
        // so neighbours in the same family stay distinguishable.
        let jitter = (fnv1a(node.name.as_bytes()) % 7) as f64 * 0.012;
        let parity = if tile.sibling_rank.is_multiple_of(2) {
            0.0
        } else {
            0.07
        };
        Hsl {
            h: self.hue(tile).unwrap_or(0.0).rem_euclid(360.0),
            s: 0.5,
            l: 0.30 + parity + jitter,
        }
    }

    /// (background, line colour) for a directory's chrome. Deeper levels get
    /// slightly lighter backgrounds so nesting reads at a glance.
    fn dir_colors(&self, tile: &Tile) -> (Hsl, Hsl) {
        let (h, s) = match self.hue(tile) {
            Some(h) => (h.rem_euclid(360.0), 0.35),
            None => (0.0, 0.0),
        };
        let depth = tile.depth.min(6) as f64;
        (
            Hsl {
                h,
                s,
                l: 0.10 + 0.035 * depth,
            },
            Hsl {
                h,
                s: s + 0.15,
                l: 0.55,
            },
        )
    }

    fn paint(&self, buf: &mut Buffer, r: Rect, symbol: &str, style: Style) {
        for y in r.top()..r.bottom() {
            for x in r.left()..r.right() {
                buf[(x, y)].set_symbol(symbol).set_style(style);
            }
        }
    }

    fn c(&self, hsl: Hsl) -> Color {
        self.color_mode.convert(hsl.to_color())
    }

    fn draw_file(&self, buf: &mut Buffer, tile: &Tile, selected: bool, in_sel_dir: bool) {
        let collapsed = tile.kind == TileKind::CollapsedDir;
        let (bg, light, texture_fg) = match (&self.color_fn, tile.kind) {
            (Some(f), TileKind::File) => (f(self.tree, tile), 0.4, None),
            _ => {
                let mut c = if collapsed {
                    let (bg, _) = self.dir_colors(tile);
                    Hsl {
                        l: bg.l + 0.12,
                        ..bg
                    }
                } else {
                    self.file_color(tile)
                };
                if in_sel_dir {
                    c.l += 0.18;
                }
                let tex = Hsl { l: c.l + 0.12, ..c }.to_color();
                (c.to_color(), c.l, Some(tex))
            }
        };
        let style = if selected {
            self.selected_style
        } else {
            let fg = if light > 0.55 {
                Color::Black
            } else {
                Color::Rgb(235, 235, 235)
            };
            Style::default().bg(bg).fg(fg)
        };
        let style = self.convert_style(style);
        // Collapsed directories get a texture so they don't read as files.
        match texture_fg {
            Some(tex) if collapsed && !selected => {
                self.paint(buf, tile.rect, "░", style.fg(self.color_mode.convert(tex)))
            }
            _ => self.paint(buf, tile.rect, " ", style),
        }
        self.draw_label(buf, tile, style);
    }

    /// A directory in [`Nesting::Header`] mode: a one-row title bar
    /// (`name/ size`) on top of its contents. The title bar is drawn in the
    /// selected style when the directory is selected, and brightened when it
    /// encloses the selection.
    fn draw_dir(&self, buf: &mut Buffer, tile: &Tile, selected: bool, on_sel_path: bool) {
        let r = tile.rect;
        let (body, mut text) = self.dir_colors(tile);
        let mut bar = Hsl { l: 0.22, ..body };
        if on_sel_path {
            bar.l = 0.32;
            text.l = 0.85;
        }
        // Background for the interior (children normally cover all of it).
        self.paint(buf, r, " ", Style::default().bg(self.c(body)));

        let style = if selected {
            self.convert_style(self.selected_style)
        } else {
            let st = Style::default().bg(self.c(bar)).fg(self.c(text));
            st.add_modifier(Modifier::BOLD)
        };
        self.paint(buf, Rect::new(r.x, r.y, r.width, 1), " ", style);
        if self.labels != Labels::None && r.width > 2 {
            let node = self.tree.node(tile.node);
            let full = format!(" {}/ {} ", node.name, format_size(node.size));
            let w = r.width as usize;
            let title = if full.chars().count() <= w {
                full
            } else {
                fit(&format!(" {}/", node.name), w)
            };
            buf.set_string(r.x, r.y, title, style);
        }
    }

    fn convert_style(&self, mut s: Style) -> Style {
        s.fg = s.fg.map(|c| self.color_mode.convert(c));
        s.bg = s.bg.map(|c| self.color_mode.convert(c));
        s
    }

    fn draw_label(&self, buf: &mut Buffer, tile: &Tile, style: Style) {
        let r = tile.rect;
        // Leave the last column blank so labels of adjacent tiles don't merge.
        if self.labels == Labels::None || r.width < 4 {
            return;
        }
        let node = self.tree.node(tile.node);
        let w = r.width as usize - 1;
        let name = if tile.kind == TileKind::CollapsedDir {
            format!("{}/", node.name)
        } else {
            node.name.clone()
        };
        buf.set_string(r.x, r.y, fit(&name, w), style);
        let size = format_size(node.size);
        if self.labels == Labels::NameAndSize && r.height >= 2 && size.len() <= w {
            buf.set_string(r.x, r.y + 1, size, style.add_modifier(Modifier::DIM));
        }
    }
}

/// Truncate to `w` columns with an ellipsis.
fn fit(s: &str, w: usize) -> String {
    if s.chars().count() <= w {
        s.to_string()
    } else if w == 0 {
        String::new()
    } else {
        let cut: String = s.chars().take(w - 1).collect();
        format!("{cut}…")
    }
}

impl<'a> StatefulWidget for TreeMap<'a> {
    type State = TreeMapState;

    fn render(mut self, area: Rect, buf: &mut Buffer, state: &mut TreeMapState) {
        let area = match self.block.take() {
            Some(b) => {
                let inner = b.inner(area);
                b.render(area, buf);
                inner
            }
            None => area,
        };
        state.ensure_layout(self.tree, area, &self.layout);
        let layout = state.layout.as_ref().expect("layout computed");
        let selected = state.selected;
        // Directories enclosing the selection (not including it).
        let sel_ancestors: HashSet<NodeId> = match (selected, self.highlight_dir) {
            (Some(s), true) => self
                .tree
                .ancestors(s)
                .skip(1)
                .take_while(|&a| a != layout.root)
                .collect(),
            _ => HashSet::new(),
        };
        let flat_sel_dir = selected
            .and_then(|s| self.tree.parent(s))
            .filter(|&d| d != layout.root);

        // Pre-order: directories are painted before their contents.
        for tile in &layout.tiles {
            let is_sel = Some(tile.node) == selected;
            match tile.kind {
                TileKind::Dir => {
                    self.draw_dir(buf, tile, is_sel, sel_ancestors.contains(&tile.node))
                }
                TileKind::File | TileKind::CollapsedDir => {
                    let in_sel_dir = self.highlight_dir
                        && layout.nesting == Nesting::Flat
                        && flat_sel_dir
                            .is_some_and(|d| self.tree.is_ancestor_or_self(d, tile.node));
                    self.draw_file(buf, tile, is_sel, in_sel_dir)
                }
            }
        }
    }
}

/// Screen-space direction for keyboard navigation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

/// View root + selection + cached layout for a [`TreeMap`].
///
/// The layout is recomputed only when the tree, view root, area or layout
/// options change, so rendering an unchanged view is just painting cells.
/// Navigation methods operate on the layout from the most recent render.
///
/// Navigation follows the hierarchy:
/// [`move_selection`](Self::move_selection) moves between siblings,
/// [`select_child`](Self::select_child) and [`select_parent`](Self::select_parent)
/// go down and up a level, and [`zoom_in`](Self::zoom_in) and
/// [`zoom_out`](Self::zoom_out) change which directory fills the view.
#[derive(Debug)]
pub struct TreeMapState {
    root: NodeId,
    selected: Option<NodeId>,
    layout: Option<TreemapLayout>,
    layout_key: Option<(u64, NodeId, Rect, u64, Nesting)>,
}

impl TreeMapState {
    pub fn new(root: NodeId) -> Self {
        TreeMapState {
            root,
            selected: None,
            layout: None,
            layout_key: None,
        }
    }

    /// The directory currently filling the view.
    pub fn root(&self) -> NodeId {
        self.root
    }

    pub fn set_root(&mut self, root: NodeId) {
        self.root = root;
    }

    pub fn selected(&self) -> Option<NodeId> {
        self.selected
    }

    pub fn select(&mut self, node: Option<NodeId>) {
        self.selected = node;
    }

    /// Layout from the last render (tiles, hidden stats, hit testing).
    pub fn layout(&self) -> Option<&TreemapLayout> {
        self.layout.as_ref()
    }

    /// Force recomputation on next render.
    pub fn invalidate(&mut self) {
        self.layout_key = None;
    }

    /// Select the innermost tile under an absolute terminal cell (e.g. a mouse
    /// click): a file, a collapsed directory, or a directory's frame.
    pub fn select_at(&mut self, x: u16, y: u16) -> Option<NodeId> {
        let hit = self.layout.as_ref()?.tile_at(x, y)?.node;
        self.selected = Some(hit);
        Some(hit)
    }

    /// Move to the nearest sibling in `dir`. If there's none, try the parent's
    /// siblings, and so on outward. In [`Nesting::Flat`] all tiles are
    /// candidates. Returns whether the selection moved.
    pub fn move_selection(&mut self, dir: Direction) -> bool {
        let Some(layout) = &self.layout else {
            return false;
        };
        let Some(mut cur) = self.selected.and_then(|s| layout.tile_of(s)) else {
            self.selected = layout.tiles.first().map(|t| t.node);
            return self.selected.is_some();
        };
        loop {
            let parent = cur.parent;
            let candidates = layout
                .tiles
                .iter()
                .filter(|t| layout.nesting == Nesting::Flat || t.parent == parent);
            if let Some(next) = nearest(cur.rect, dir, candidates.filter(|t| t.node != cur.node)) {
                self.selected = Some(next);
                return true;
            }
            match layout.tile_of(parent) {
                Some(p) if layout.nesting != Nesting::Flat => cur = p,
                _ => return false,
            }
        }
    }

    /// Descend: select the largest child of the selected directory.
    pub fn select_child(&mut self) -> bool {
        let Some(layout) = &self.layout else {
            return false;
        };
        let Some(sel) = self.selected else {
            return false;
        };
        match layout.children_of(sel).next() {
            Some(child) => {
                self.selected = Some(child.node);
                true
            }
            None => false,
        }
    }

    /// Ascend: select the directory enclosing the selection, if it's drawn
    /// (i.e. not the view root).
    pub fn select_parent(&mut self) -> bool {
        let Some(layout) = &self.layout else {
            return false;
        };
        let Some(tile) = self.selected.and_then(|s| layout.tile_of(s)) else {
            return false;
        };
        if layout.tile_of(tile.parent).is_some() {
            self.selected = Some(tile.parent);
            true
        } else {
            false
        }
    }

    /// Make the selected directory (or the selected file's directory) fill the
    /// view. Returns whether the view changed.
    pub fn zoom_in(&mut self, tree: &DiskTree) -> bool {
        let Some(sel) = self.selected else {
            return false;
        };
        let target = if tree.node(sel).is_dir() {
            sel
        } else {
            tree.parent(sel).unwrap_or(sel)
        };
        if target == self.root || !tree.is_ancestor_or_self(self.root, target) {
            return false;
        }
        self.root = target;
        if sel == target {
            self.selected = None; // re-picked as the largest child on render
        }
        true
    }

    /// Move the view root up one level and select the directory we came from.
    pub fn zoom_out(&mut self, tree: &DiskTree) -> bool {
        match tree.parent(self.root) {
            Some(p) => {
                self.selected = Some(self.root);
                self.root = p;
                true
            }
            None => false,
        }
    }

    fn ensure_layout(&mut self, tree: &DiskTree, area: Rect, opts: &LayoutOptions) {
        let key = (
            tree.id(),
            self.root,
            area,
            opts.cell_aspect.to_bits(),
            opts.nesting,
        );
        if self.layout_key != Some(key) {
            self.layout = Some(TreemapLayout::compute(tree, self.root, area, opts));
            self.layout_key = Some(key);
        }
        // Keep the selection on a visible tile: the selected node itself, else
        // its nearest drawn ancestor (e.g. after a resize hid it), else its
        // largest drawn descendant (a directory isn't drawn in flat mode), else
        // the largest tile.
        let layout = self.layout.as_ref().expect("just set");
        self.selected = self
            .selected
            .and_then(|s| {
                tree.ancestors(s)
                    .find(|&a| layout.tile_of(a).is_some())
                    .or_else(|| {
                        let t = layout
                            .tiles
                            .iter()
                            .find(|t| tree.is_ancestor_or_self(s, t.node));
                        t.map(|t| t.node)
                    })
            })
            .or_else(|| layout.tiles.first().map(|t| t.node));
    }
}

/// The candidate closest to `from` in `dir`. Tiles overlapping `from`'s span
/// on the other axis win, then the nearest, then the best-aligned centre.
fn nearest<'t>(
    from: Rect,
    dir: Direction,
    candidates: impl Iterator<Item = &'t Tile>,
) -> Option<NodeId> {
    let c = from;
    let (cx, cy) = (
        2 * c.x as i32 + c.width as i32,
        2 * c.y as i32 + c.height as i32,
    );
    candidates
        .filter_map(|t| {
            let r = t.rect;
            let (tx, ty) = (
                2 * r.x as i32 + r.width as i32,
                2 * r.y as i32 + r.height as i32,
            );
            let (ahead, gap, overlaps, misalign) = match dir {
                Direction::Right => (
                    r.left() >= c.right(),
                    r.left() as i32 - c.right() as i32,
                    r.top() < c.bottom() && c.top() < r.bottom(),
                    (cy - ty).abs(),
                ),
                Direction::Left => (
                    r.right() <= c.left(),
                    c.left() as i32 - r.right() as i32,
                    r.top() < c.bottom() && c.top() < r.bottom(),
                    (cy - ty).abs(),
                ),
                Direction::Down => (
                    r.top() >= c.bottom(),
                    r.top() as i32 - c.bottom() as i32,
                    r.left() < c.right() && c.left() < r.right(),
                    (cx - tx).abs(),
                ),
                Direction::Up => (
                    r.bottom() <= c.top(),
                    c.top() as i32 - r.bottom() as i32,
                    r.left() < c.right() && c.left() < r.right(),
                    (cx - tx).abs(),
                ),
            };
            ahead.then_some(((!overlaps) as i32, gap, misalign, t.node))
        })
        .min()
        .map(|(_, _, _, n)| n)
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325u64, |h, &b| {
        (h ^ b as u64).wrapping_mul(0x100000001b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::DiskTreeBuilder;

    fn row(buf: &Buffer, y: u16) -> String {
        (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect()
    }

    fn sample() -> (DiskTree, NodeId, NodeId, NodeId) {
        let mut b = DiskTreeBuilder::new("/r");
        let r = b.root();
        let src = b.add_dir(r, "src");
        let main_rs = b.add_file(src, "main.rs", 2000);
        b.add_file(src, "lib.rs", 1000);
        let target = b.add_file(r, "target.bin", 5000);
        (b.finish(), src, main_rs, target)
    }

    #[test]
    fn flat_is_default_and_navigates_spatially() {
        let (tree, src, main_rs, target) = sample();
        let area = Rect::new(0, 0, 40, 10);
        let mut buf = Buffer::empty(area);
        let mut state = TreeMapState::new(tree.root());
        TreeMap::new(&tree).render(area, &mut buf, &mut state);

        // Only files are drawn, with globally exact areas.
        let layout = state.layout().unwrap();
        assert_eq!(layout.nesting, Nesting::Flat);
        assert!(layout.tiles.iter().all(|t| t.kind == TileKind::File));
        assert_eq!(state.selected(), Some(target));
        let t = layout.tile_of(target).unwrap().rect;
        assert_eq!(t, Rect::new(0, 0, 25, 10)); // exactly 5/8 of the view
        assert_eq!(buf[(t.x, t.y)].bg, Color::Rgb(250, 250, 250));
        let m = layout.tile_of(main_rs).unwrap();
        assert!((m.exact.area() - 400.0 * 2.0 / 8.0).abs() < 1e-9); // 2000/8000

        // Arrows cross directory boundaries; there are no directory tiles to
        // enter or leave.
        assert!(state.move_selection(Direction::Right));
        assert_eq!(tree.parent(state.selected().unwrap()), Some(src));
        assert!(!state.select_child());
        assert!(!state.select_parent());

        // Zooming in on a file shows its directory; zooming out lands on that
        // directory's largest file, since the directory itself isn't drawn.
        assert!(state.zoom_in(&tree));
        assert_eq!(state.root(), src);
        assert!(state.zoom_out(&tree));
        TreeMap::new(&tree).render(area, &mut buf, &mut state);
        assert_eq!(state.selected(), Some(main_rs));
    }

    #[test]
    fn header_mode_nests_and_navigates_hierarchy() {
        let (tree, src, main_rs, target) = sample();
        let area = Rect::new(0, 0, 40, 10);
        let mut buf = Buffer::empty(area);
        let mut state = TreeMapState::new(tree.root());
        let map = || TreeMap::new(&tree).nesting(Nesting::Header);
        map().render(area, &mut buf, &mut state);
        assert_eq!(state.selected(), Some(target));

        // `src` is its own rectangle: a title bar, then its files.
        let s = state.layout().unwrap().tile_of(src).unwrap().clone();
        assert_eq!((s.kind, s.rect), (TileKind::Dir, Rect::new(25, 0, 15, 10)));
        assert_eq!(s.inner, Rect::new(25, 1, 15, 9));
        assert!(row(&buf, 0).contains(" src/ 2.9 KiB "), "{}", row(&buf, 0));
        let m = state.layout().unwrap().tile_of(main_rs).unwrap();
        assert!(s.inner.contains(m.rect.as_position()) && m.rect.bottom() <= s.inner.bottom());

        // Siblings, then down into the directory, then back up.
        assert!(state.move_selection(Direction::Right));
        assert_eq!(state.selected(), Some(src));
        assert!(!state.move_selection(Direction::Right)); // nothing further right
        assert!(state.select_child());
        assert_eq!(state.selected(), Some(main_rs));
        assert!(state.select_parent());
        assert_eq!(state.selected(), Some(src));
        assert!(!state.select_parent()); // parent is the view root

        // Zoom into src: it fills the view, its largest child selected.
        assert!(state.zoom_in(&tree));
        assert_eq!(state.root(), src);
        map().render(area, &mut buf, &mut state);
        assert_eq!(state.selected(), Some(main_rs));
        let main = state.layout().unwrap().tile_of(main_rs).unwrap();
        assert!((main.exact.area() - 400.0 * 2.0 / 3.0).abs() < 1e-9);

        // Zoom out selects the directory we came from.
        assert!(state.zoom_out(&tree));
        map().render(area, &mut buf, &mut state);
        assert_eq!(state.selected(), Some(src));
        assert!(!state.zoom_out(&tree));
    }

    #[test]
    fn every_mode_and_colour_depth_renders() {
        let mut b = DiskTreeBuilder::new("/r");
        let r = b.root();
        let d = b.add_dir(r, "dir");
        let dd = b.add_dir(d, "sub");
        b.add_file(dd, "x", 10);
        b.add_file(d, "y", 30);
        b.add_file(r, "z", 50);
        let tree = b.finish();
        for nesting in [Nesting::Header, Nesting::Flat] {
            for mode in [
                ColorMode::TrueColor,
                ColorMode::Indexed256,
                ColorMode::Basic16,
            ] {
                for area in [
                    Rect::new(0, 0, 60, 20),
                    Rect::new(0, 0, 3, 2),
                    Rect::new(0, 0, 1, 1),
                ] {
                    let mut buf = Buffer::empty(area);
                    let mut state = TreeMapState::new(tree.root());
                    TreeMap::new(&tree)
                        .nesting(nesting)
                        .color_mode(mode)
                        .render(area, &mut buf, &mut state);
                    for dir in [
                        Direction::Up,
                        Direction::Down,
                        Direction::Left,
                        Direction::Right,
                    ] {
                        state.move_selection(dir);
                    }
                    state.select_child();
                    state.select_parent();
                }
            }
        }
    }
}

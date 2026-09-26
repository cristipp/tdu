# ydu — ratatui disk-usage treemap API

Every file is a rectangle sized by bytes. Two modes:

- **`Nesting::Flat` (default):** only files are drawn, and every file's area is
  exactly proportional to its size across the whole view.
- **`Nesting::Header`:** one rectangle per directory and per file, nested
  recursively. Each directory has a one-row title bar above its children, and
  siblings split their parent's interior in exact proportion to their sizes.

## Layers

```
tree    DiskTree            immutable arena of sizes (scan() or DiskTreeBuilder)
  │
layout  TreemapLayout       pure squarified layout: exact f64 rects → snapped cell Rects
  │
widget  TreeMap<'a>         ratatui StatefulWidget: paints tiles and title bars
        TreeMapState        view root, selection, navigation, hit testing, layout cache
```

Each layer only depends on the one above it. You can use `layout` without
ratatui widgets (for tests, or other renderers), and feed `widget` from any
data source by building a `DiskTree` by hand.

## Public API

```rust
// ── tree ────────────────────────────────────────────────────────────
pub struct NodeId;                          // Copy index into a DiskTree
pub enum   NodeKind { File, Dir, Symlink, Other }
pub struct Node { name, kind, size, file_count, parent, children /* size desc */ }

pub struct DiskTree { pub errors: Vec<(PathBuf, io::Error)>, .. }
impl DiskTree {
    fn root(&self) -> NodeId;
    fn node(&self, NodeId) -> &Node;
    fn parent(&self, NodeId) -> Option<NodeId>;
    fn ancestors(&self, NodeId) -> impl Iterator<Item = NodeId>;
    fn child_toward(&self, ancestor, descendant) -> Option<NodeId>;
    fn path_of(&self, NodeId) -> PathBuf;
}

pub struct DiskTreeBuilder;                 // for non-filesystem sources and tests
impl DiskTreeBuilder {
    fn new(root_path) -> Self;
    fn add_dir(&mut self, parent, name) -> NodeId;
    fn add_file(&mut self, parent, name, size) -> NodeId;
    fn add_entry_os(&mut self, parent, OsString, kind, size) -> NodeId; // non-UTF-8 safe
    fn finish(self) -> DiskTree;            // sums dir sizes, sorts children
}

pub struct ScanOptions { size_mode: SizeMode /* Apparent | Allocated */,
                         one_file_system: bool,
                         dedupe_hardlinks: bool /* default true */,
                         skip_virtual_fs: bool  /* default true */ }
pub fn scan(path, &ScanOptions) -> io::Result<DiskTree>;
pub fn scan_with_progress(path, &ScanOptions, &ScanProgress) -> io::Result<DiskTree>;
pub struct ScanProgress;                    // Sync; shared with a UI thread
impl ScanProgress {
    fn new() -> Self;
    fn snapshot(&self) -> ProgressSnapshot;  // { entries, bytes, dirs, errors, fraction, current }
    fn cancel(&self);                        // scan returns ErrorKind::Interrupted
}

// ── layout ──────────────────────────────────────────────────────────
pub fn squarify(weights_desc: &[f64], rect: RectF) -> Vec<RectF>;
pub enum   Nesting { Flat /* default */, Header }
pub struct LayoutOptions { cell_aspect: f64 /* cell h/w, default 2.0 */, nesting: Nesting }
pub enum   TileKind { File, Dir, CollapsedDir }
pub struct Tile { node, parent, kind: TileKind,
                  rect: Rect, inner: Rect /* dir interior */, exact: RectF,
                  depth, group, sibling_rank }
pub struct HiddenStats { files: u64, bytes: u64 }   // not individually drawn
pub struct TreemapLayout { area, root, nesting, tiles: Vec<Tile> /* pre-order */, hidden }
impl TreemapLayout {
    fn compute(&DiskTree, root: NodeId, area: Rect, &LayoutOptions) -> Self;
    fn tile_at(&self, x, y) -> Option<&Tile>;      // O(1), innermost tile
    fn tile_of(&self, NodeId) -> Option<&Tile>;
    fn children_of(&self, NodeId) -> impl Iterator<Item = &Tile>;
}

// ── widget ──────────────────────────────────────────────────────────
TreeMap::new(&tree)
    .block(Block)                            // optional outer frame
    .nesting(Nesting::Flat)                  // Flat (default) | Header
    .color_by(ColorBy::TopLevel | Extension | Depth)
    .color_fn(|tree, tile| Color)            // full override
    .color_mode(ColorMode::detect())         // TrueColor | Indexed256 | Basic16
    .labels(Labels::None | Name | NameAndSize)
    .cell_aspect(2.0)
    .highlight_dir(true)                     // flat: brighten the selection's dir; header: its title bars
    .selected_style(Style);

impl StatefulWidget for TreeMap<'_> { type State = TreeMapState; }

impl TreeMapState {
    fn new(root) -> Self;
    fn root/set_root, selected/select;
    fn move_selection(&mut self, Direction) -> bool;  // nearest sibling that way, else bubble up
    fn select_child(&mut self) -> bool;               // into a directory (largest child)
    fn select_parent(&mut self) -> bool;              // out to the enclosing directory
    fn select_at(&mut self, x, y) -> Option<NodeId>;  // mouse: innermost tile
    fn zoom_in(&mut self, &DiskTree) -> bool;         // selected dir (or file's dir) fills view
    fn zoom_out(&mut self, &DiskTree) -> bool;        // up one level, selects where we came from
    fn layout(&self) -> Option<&TreemapLayout>;       // tiles and hidden stats from last render
    fn invalidate(&mut self);
}
```

Usage:

```rust
let tree = scan(path, &ScanOptions::default())?;
let mut state = TreeMapState::new(tree.root());
terminal.draw(|f| f.render_stateful_widget(TreeMap::new(&tree), f.area(), &mut state))?;
// on key: state.move_selection(Direction::Right); state.zoom_in(&tree); ...
```

## Key decisions

**Flat by default.** In `Flat` mode, directories aren't drawn. Children share
their parent's *unsnapped* rect, so every file's area is exactly
`size / total × view_area` (`Tile::exact`, tested). Directory extent is shown
with colour: one hue per top-level subtree, plus a brightened wash over the
selected file's directory.

**Header mode: nested rectangles, one per node.** Layout recurses. The view
root's children are squarified into the full area. Each directory's snapped
rectangle loses one title row, and its children are squarified into the
remaining *snapped* interior. Snapping each level before recursing means
children always tile their parent's interior exactly. Tiles are emitted in
pre-order, so painting in order draws title bars before contents, and the
hit-test map ends up holding the innermost tile at each cell.

A title row is the cheapest chrome that still labels a directory. An earlier
design also offered full 1-cell borders (`Framed`), but they cost two rows and
two columns per level, and it was dropped.

**The trade-off.** Header siblings are exactly proportional to each other
(tested). Across directories, each title row is a real cost: a directory of
N bytes shows its files in *less* than N bytes' worth of area. That's the
standard price of a nested treemap, and it's why `Flat` is the default.

**Collapsing.** In `Header` mode, a directory whose rectangle is smaller than
3×2 is drawn as one labelled, textured block (`TileKind::CollapsedDir`), and
its files count as not shown. Zooming in reveals the contents.

**Hierarchical navigation.** In a nested map, "the nearest rectangle to the
right" is ambiguous (a directory contains its children), so arrows move among
*siblings*. At an edge, the move bubbles up to the parent's siblings.
`select_child` and `select_parent` move in depth, and zoom changes the view root.
In `Flat` mode, arrows use every tile.

**Snapping without gaps.** Each exact rect is snapped to cells by rounding its
edges. `squarify` computes positions from cumulative sums and pins the last edge
to the container edge, so neighbours share *bit-identical* edge values. After
rounding, tiles cover the area with no gaps or overlaps (tested). Each edge moves
by at most half a cell, so the error is at most about `(w + h) / 2` cells.

**Honesty about what can't be drawn.** Something smaller than one cell can't be
drawn in proportion. Instead of inflating it to a whole cell (which would lie),
it goes into `HiddenStats`, which the app shows in the status bar. Rounding is
monotone, so if a directory snaps to nothing, so does everything inside it.
That lets layout prune the whole subtree, and the work per frame is bounded by
the number of screen cells, not the number of files.

**Non-square cells.** Squarify runs in physical units (a cell is
`1 × cell_aspect`). This makes tiles look square on screen and changes only
their shapes. Every cell has the same area, so proportionality holds.

**State owns the cache.** `TreeMapState` caches the layout, keyed by
`(tree id, view root, area, aspect, nesting)`. Redrawing an unchanged view only repaints
cells, and navigation and hit testing use exactly what's on screen. Each
`DiskTree` gets a unique id, so swapping in a rescanned tree invalidates the
cache automatically.

**Scanning.** It uses `std` plus `libc::statfs`, and an explicit stack (so deep
trees can't overflow the call stack). Unreadable entries go into
`DiskTree::errors` instead of aborting the scan.

**Linux + macOS.** Scanning handles platform quirks in one place
(`tree::sys`):

- **Only the root is resolved through symlinks.** macOS's `/tmp`, `/var` and
  `/etc` are symlinks. Nothing below the root is followed.
- **Each directory is visited once per `(dev, inode)`.** This fixes
  double-counting through macOS APFS firmlinks (`/Users` ==
  `/System/Volumes/Data/Users`, verified to share dev+inode), and stops
  infinite loops through Linux bind mounts.
- **Virtual filesystems are detected with `statfs`**, only when a directory's
  device differs from its parent's (a mount point), so it costs one syscall per
  mount. Linux compares `f_type` magic numbers (proc, sysfs, cgroup, …);
  macOS compares `f_fstypename` (devfs, autofs, fdesc). The scan root is
  never skipped.
- **Filenames are `String` for display.** When the on-disk name isn't valid
  UTF-8 (Linux), the exact `OsString` is kept in `Node::raw_name` so
  `path_of` still produces an openable path.
- **Hard links are deduped by `(dev, inode)`** for files with `nlink > 1`.
  Allocated size is `st_blocks × 512` on both platforms.

**Colour depth.** The widget computes colours in HSL→RGB and down-converts
through `ColorMode` at paint time. The 256-colour mode uses the nearest xterm
cube or grey-ramp entry. The 16-colour mode maps by *hue family*, since
nearest-RGB would turn the dark tile colours almost all black. The library
never reads the environment; `ColorMode::detect()` is opt-in.

**Scan progress.** `ScanProgress` is a bag of atomics that the scanner bumps
with relaxed adds per entry, plus one uncontended mutex per *directory* for
the current path, so observing costs the scan almost nothing. Cancellation is
checked per directory and every 4096 entries, so even a single huge directory
stops promptly. The progress *fraction* can't be exact without knowing the
total up front, and a pre-count pass would double the I/O. So it's a
structural estimate: the root is worth 1.0, each directory splits its share
evenly among its subdirectories, and a leaf directory banks its share once it
has been read. It's monotone (tested from a watching thread), and it's forced
to exactly 1.0 at the end to absorb float drift. An estimate from bytes vs. the
filesystem's used space was rejected. It breaks on APFS volume groups
(`statvfs("/")` reports the sealed system volume), on clones and snapshots,
and on any scan that isn't a whole mount.

## Binary

`cargo run --release -- [--nesting flat|header] [--allocated] [-x] [--no-skip-virtual] [--color top|ext|depth] [--colors truecolor|256|16] PATH`

Keys: `←↓↑→`/`hjkl` move · `Enter` into dir (flat: zoom) · `Backspace` up ·
`z`/`Z` zoom · `n` nesting mode · `c` colours · `⌘C` (macOS) / `Ctrl-Shift-C`
(Linux) / `y` copy absolute path · double-click or `o` open · `q` / `Ctrl-C`
quit.

The scan runs on a worker thread inside the TUI, behind a progress screen
(gauge, counters, rate, current directory) that redraws every 80 ms. It
appears only after 150 ms, so small scans don't flash. `q`, `Esc` or `Ctrl-C`
cancels, and a nonexistent path is reported before the terminal is taken
over.

**Desktop integration** (`src/platform.rs`, binary only, so the library stays
free of process spawning):

- **Copy key.** Cmd-C on macOS and Ctrl-Shift-C on Linux match each
  platform's copy convention. Terminals report those modifiers only through
  the kitty keyboard protocol, and usually bind the keys themselves. So ydu
  asks `supports_keyboard_enhancement()`, pushes `DISAMBIGUATE_ESCAPE_CODES`
  only if it's supported (and pops it on exit), and always accepts `y` as a
  fallback that every terminal passes through. The help line shows which
  keys are live. `is_copy_key` is a pure function, unit-tested per OS.
- **Copy.** Locally, the text is piped to `pbcopy`, `wl-copy`, `xclip` or
  `xsel`, whichever is found first for the session (Wayland or X11). Over SSH,
  or with no tool installed, ydu writes OSC 52 to the terminal. That way the
  path lands on the machine you're sitting at, not the remote one.
- **Open.** `open` on macOS, `xdg-open` (or `gio open`) on Linux. The path is
  passed as a single argument, never through a shell.
- **Never block, never draw over the TUI.** Children get null stdio and are
  waited on from a thread, which reports a `Notice` over a channel. The event
  loop polls every 200 ms, so notices show up without waiting for input.
- **Double-click.** crossterm reports only individual presses, so
  `ClickTracker` treats two presses on the same *tile* within 500 ms as a
  double-click. Using the tile rather than the exact cell tolerates slight
  pointer movement.
- **Absolute paths.** `scan` makes the root absolute with
  `std::path::absolute`, which leaves symlinks as written, so `path_of` is
  always absolute.

`cargo run --example snapshot -- PATH [--nesting MODE]` renders one frame headlessly as text.

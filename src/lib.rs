//! Disk-usage treemap for [ratatui].
//!
//! Three layers, each usable on its own:
//!
//! * [`tree`]   — [`DiskTree`]: immutable arena of sizes, from [`scan`] or [`DiskTreeBuilder`].
//! * [`layout`] — [`TreemapLayout`]: pure squarified layout; every file's area is
//!   exactly proportional to its size, then snapped to cells without gaps.
//! * [`widget`] — [`TreeMap`] (a `StatefulWidget`) + [`TreeMapState`]
//!   (view root, selection, navigation, hit testing, cached layout).
//!
//! ```no_run
//! use tdu::*;
//! let tree = scan(".", &ScanOptions::default()).unwrap();
//! let mut state = TreeMapState::new(tree.root());
//! // inside terminal.draw(|f| ...):
//! // f.render_stateful_widget(TreeMap::new(&tree), f.area(), &mut state);
//! ```

pub mod color;
pub mod layout;
pub mod tree;
pub mod widget;

pub use color::ColorMode;
pub use layout::{
    HiddenStats, LayoutOptions, Nesting, RectF, Tile, TileKind, TreemapLayout, squarify,
};
pub use tree::{
    DiskTree, DiskTreeBuilder, Node, NodeId, NodeKind, ProgressSnapshot, ScanOptions, ScanProgress,
    SizeMode, format_size, scan, scan_with_progress,
};
pub use widget::{ColorBy, ColorFn, Direction, Labels, TreeMap, TreeMapState};

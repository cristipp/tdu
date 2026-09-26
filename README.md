# ydu

A disk-usage **treemap** for the terminal, built on [ratatui](https://ratatui.rs).
Every file is a rectangle whose area is proportional to its size, so you can
spot what's eating your disk at a glance.

Two modes (`--nesting`, or press `n`):

**`flat`** (default): only files are drawn, and every file's area is exactly
proportional to its size across the whole screen. The selected file's
directory is brightened to show where it belongs.

```
libsyn-8bb38… libdarlin… libratatu… libratat… dep-grap…    dep-gr…    libsyn-b1e1…
8.9 MiB       6.7 MiB    6.6 MiB    6.6 MiB   6.4 MiB      5.4 MiB    8.7 MiB

libsyn-b… libdar… libsyn… ydu-… libcr… libde… dep-gra… qu… dep-gr…    libder…
5.0 MiB   4.2 MiB 3.6 MiB                     6.3 MiB      5.4 MiB    4.2 MiB
```

**`header`**: directories are drawn too. Each is its own rectangle with a
one-row title bar (`name/ size`), and its contents are nested inside,
recursively.

```
 target/ 413 MiB
 debug/ 288 MiB                                                        release/ 125 MiB
 deps/ 190 MiB                                 incremental/ 79 MiB     deps/ 115 MiB
libsyn-8bb… libdarl… librata… librata… libsy…  ydu-1t98wic… ydu-2rbj9…libsyn-b1e1…
8.9 MiB     6.7 MiB  6.6 MiB  6.6 MiB          s-hmn4sr1rw… s-hmn4mhd…8.7 MiB
```

*(Real output from `cargo run --example snapshot -- . [--nesting header]`,
with colours stripped. In the terminal, each top-level folder has its own
hue.)*

It comes as both:

- a **library**: a reusable `TreeMap` ratatui widget, a layout engine and a filesystem scanner
- a **binary**: `ydu`, an interactive disk-usage explorer

## Install / build

```sh
cargo build --release
./target/release/ydu ~/Downloads
```

Requires Rust 1.88+ (edition 2024). Built on `ratatui 0.30`; crossterm comes via `ratatui::crossterm`, so there are no version-mismatch issues.

## Platforms

**Linux** and **macOS** are supported and tested: x86_64 and aarch64 in CI,
with Linux also tested in Docker as a non-root user. Things each platform
handles:

| | Linux | macOS |
|---|---|---|
| Virtual filesystems skipped | `proc`, `sysfs`, `cgroup`, `debugfs`, `tracefs`, `bpf`, … (`/proc/kcore` alone claims ~128 TiB) | `devfs` (`/dev`), `autofs` (network automounts can hang), `fdesc` |
| Same directory reachable twice | bind mounts (a loop would never end) | APFS firmlinks: `/Users` is `/System/Volumes/Data/Users` |
| Symlinked scan root | followed | followed: `/tmp`, `/var`, `/etc` → `/private/…` |
| Non-UTF-8 filenames | shown lossily, exact bytes kept for paths | n/a (APFS requires UTF-8) |
| Allocated size (`--allocated`) | `st_blocks × 512` | `st_blocks × 512` |
| Terminal colours | truecolor / 256 / 16 (Linux console) | truecolor / 256 (older Terminal.app) |

Colour depth is detected from `COLORTERM` and `TERM`. Override it with
`--colors`.

On macOS, grant your terminal **Full Disk Access** (System Settings → Privacy &
Security) to scan protected folders such as `~/Library/Mail`. Otherwise they
are counted as *unreadable* in the header. To see what's actually on your data
volume, scan `/System/Volumes/Data`.

Other Unix systems should build, but without virtual-filesystem detection.
Windows builds without the Unix-only features.

## Usage

```
ydu [OPTIONS] [PATH]
```

| Flag | Meaning |
|---|---|
| `--allocated` | Use on-disk allocated size (like `du`) instead of file length |
| `-x`, `--one-file-system` | Don't descend into other mounted filesystems |
| `--nesting flat\|header` | `flat`: files only, exact proportions (default). `header`: nested directory rectangles with title bars |
| `--no-skip-virtual` | Also scan `/proc`, `/sys`, `/dev`, autofs, … |
| `--color top\|ext\|depth` | Colour by top-level folder, by file extension, or by depth |
| `--colors truecolor\|256\|16` | Terminal colour depth (default: auto-detect) |

Hard-linked files are counted once. Symlinks are not followed, except for the
scan root.

### Scan progress

While the initial scan runs, ydu shows a progress screen: a gauge, live
counts (entries, bytes, directories, unreadable), throughput, and the
directory being read. Press `q`, `Esc` or `Ctrl-C` to cancel. Scans that
finish within 150 ms skip the screen. When the scan is done, the status bar
briefly shows `Scanned N entries in T`.

```
┌ ydu  scanning /home/me ─────────────────────────────────────────┐
│██████████████████████████▌          ≈ 42%                       │
│                                                                 │
│123,456 entries · 5.0 GiB · 7,890 dirs · 3 unreadable            │
│2.0 s · 61,728 entries/s                                         │
│/home/me/projects/deeply/nested/dir                              │
│q / Esc / Ctrl-C to cancel                                       │
└─────────────────────────────────────────────────────────────────┘
```

Sizes aren't known until the scan finishes, so the percentage (`≈`) is an
estimate. Each directory's share is split evenly among its subdirectories.
The estimate never goes backwards and ends at exactly 100%, but it can run
ahead or stall when one subdirectory holds most of the files. The counters
are always exact.

### Keys

| Key | Action |
|---|---|
| `←` `↓` `↑` `→` / `h` `j` `k` `l` | Flat: move to the neighbouring file. Header: move to the neighbouring sibling, or at an edge to the parent's neighbour |
| `Enter` / `]` | Header: go into the selected directory (select its largest child). On a file or in flat mode: zoom into its directory |
| `Backspace` / `[` | Go up to the enclosing directory. At the top, zoom out |
| `z` / `+` / scroll up | Zoom: the selected directory (or the file's directory) fills the screen |
| `Z` / `-` / `Esc` / right-click / scroll down | Zoom out one level |
| Left-click | Select the file, collapsed directory or title bar under the pointer |
| Double-click / `o` | Open the selection with its default app (`open` on macOS, `xdg-open` on Linux). Directories open in Finder or the file manager |
| `⌘C` (macOS) / `Ctrl-Shift-C` (Linux) / `y` | Copy the selection's absolute path to the clipboard (see below) |
| `n` | Toggle nesting mode: flat ↔ header |
| `c` | Cycle colouring: top-level folder → extension → depth |
| `q` / `Ctrl-C` | Quit |

The status bar shows the selection's path, its size (and file count for
directories), its share of the view and of the total, and how many files
aren't individually visible.

### Clipboard and opening files

| | macOS | Linux |
|---|---|---|
| Copy key | `⌘C`, or `y` | `Ctrl-Shift-C`, or `y` |
| Copy tool | `pbcopy` | `wl-copy` (Wayland), else `xclip` / `xsel` (X11) |
| Copy over SSH, or with no tool installed | OSC 52 terminal escape | OSC 52 terminal escape |
| Open (double-click, `o`) | `open` | `xdg-open`, else `gio open` |

**About ⌘C / Ctrl-Shift-C.** Terminals normally keep these keys for their own
Copy command, so the keypress never reaches the app. ydu can only see them if
the terminal supports the [kitty keyboard
protocol](https://sw.kovidgoyal.net/kitty/keyboard-protocol/) (kitty, WezTerm,
Ghostty, foot, Alacritty, iTerm2 with "Report keys using CSI u") *and* you
unbind the key from the terminal's own copy action. ydu turns the protocol on
only when the terminal says it supports it. The help line shows `⌘C/y` or
`^⇧C/y` when it's active, and just `y` otherwise (e.g. in Terminal.app or GNOME
Terminal). **`y` always works.**

Over SSH, the native tools would fill the *remote* machine's clipboard, so ydu
sends an OSC 52 escape sequence instead. It asks your terminal to set the
clipboard of the machine you're sitting at. iTerm2, kitty, WezTerm, Alacritty,
foot and recent GNOME Terminal support it. In tmux, add
`set -g set-clipboard on`.

Paths are absolute even if you started ydu with a relative path. Copy and open
run in the background, and the result (`Copied …`, `Opened …`, or the error)
appears in the status bar.

## Library usage

```toml
[dependencies]
ydu = { path = "ydu" }
```

```rust
use ydu::*;

let tree = scan("/some/path", &ScanOptions::default())?;
let mut state = TreeMapState::new(tree.root());

// Or scan on a thread and watch it (e.g. to draw a progress bar):
let progress = std::sync::Arc::new(ScanProgress::new());
let p = progress.clone();
let worker = std::thread::spawn(move || scan_with_progress("/some/path", &ScanOptions::default(), &p));
let snap = progress.snapshot();   // entries, bytes, dirs, errors, fraction, current
progress.cancel();                // -> Err(ErrorKind::Interrupted)

terminal.draw(|f| {
    let map = TreeMap::new(&tree)
        .nesting(Nesting::Flat)              // default; or Nesting::Header
        .color_by(ColorBy::Extension)
        .color_mode(ColorMode::detect())
        .labels(Labels::NameAndSize);
    f.render_stateful_widget(map, f.area(), &mut state);
})?;

// input handling
state.move_selection(Direction::Right);  // among siblings
state.select_child();                    // into a directory
state.select_parent();                   // back out
state.zoom_in(&tree);                    // directory fills the view
state.select_at(mouse_col, mouse_row);
if let Some(id) = state.selected() {
    println!("{}", tree.path_of(id).display());
}
```

The data doesn't have to come from a disk. Build a tree from anything
(archives, cloud buckets, test fixtures):

```rust
let mut b = DiskTreeBuilder::new("bucket://photos");
let dir = b.add_dir(b.root(), "2024");
b.add_file(dir, "beach.jpg", 4_200_000);
let tree = b.finish(); // sums directory sizes, sorts children
```

Or use the layout engine on its own:

```rust
let layout = TreemapLayout::compute(&tree, tree.root(), area, &LayoutOptions::default());
for tile in &layout.tiles {
    // tile.node, tile.parent, tile.kind (File | Dir | CollapsedDir),
    // tile.rect (cells), tile.inner (a directory's interior), tile.exact (f64)
}
```

## How sizes map to area

1. **Flat is exact.** In the default mode, each file's rectangle has area
   exactly `size / total × view area`, computed in continuous space.
2. **Header costs one row per directory.** In `header` mode, siblings still
   split their parent's interior in exact proportion to their sizes. But each
   directory's title bar comes out of its contents, so a deeply nested file
   looks slightly smaller than a top-level file of the same size.
3. **Gap-free snapping.** Edges are rounded to terminal cells. Siblings share
   identical edge values, so they tile their parent's interior with no gaps or
   overlaps. Each edge moves by at most half a cell.
4. **Aspect-aware.** Terminal cells are about twice as tall as they are wide,
   and layout accounts for this, so tiles *look* square. This changes only
   their shapes, never their areas.
5. **No fake tiles.** Anything smaller than a cell isn't inflated; it's
   counted as "not shown". In `header` mode, a directory too small for its
   title bar and contents becomes a single labelled `░` block, and zooming in
   reveals its contents.

See [DESIGN.md](DESIGN.md) for the full API design and rationale.

## Development

```sh
cargo test                              # layout, widget, colour, filesystem, platform tests
cargo test --bin ydu -- --ignored --test-threads=1   # copy/open spawning, with fake tools on $PATH
cargo run --example snapshot -- PATH    # render one frame as plain text

# Linux from a Mac (or anywhere with Docker), as a non-root user:
docker run --rm --user 1000:1000 -e HOME=/tmp -e CARGO_HOME=/tmp/cargo \
  -e CARGO_TARGET_DIR=/tmp/target -v "$PWD":/src:ro -w /src rust:1-slim cargo test
```

CI (`.github/workflows/ci.yml`) runs fmt, clippy, tests, a release build and a
headless smoke test on Linux and macOS (x86_64 and aarch64), plus an MSRV check.

## Project layout

```
src/tree.rs     DiskTree, DiskTreeBuilder, scan(), platform code (statfs, inodes)
src/layout.rs   squarify(), nested TreemapLayout, Tile/TileKind, Nesting, HiddenStats
src/widget.rs   TreeMap widget (tiles, title bars), TreeMapState, navigation
src/color.rs    ColorMode: truecolor / 256 / 16 detection and conversion
src/main.rs     interactive binary
src/platform.rs clipboard (pbcopy / wl-copy / xclip / OSC 52), open / xdg-open, double-click
tests/scan.rs   filesystem behaviour tests (symlinks, hard links, sparse, perms, non-UTF-8)
examples/snapshot.rs
```

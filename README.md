# tdu

treemap disk usage for the terminal.

## Build

```shell
git clone https://github.com/cristipp/tdu
cd tdu
make release
./target/release/tdu ~/Downloads
```

## Usage

```
tdu [OPTIONS] [PATH]
```

| Flag | Meaning |
|---|---|
| `--allocated` | Use on-disk allocated size (like `du`) instead of file length |
| `-x`, `--one-file-system` | Don't descend into other mounted filesystems |
| `--nesting flat` | `flat`: files only, exact proportions (default). |
| `--no-skip-virtual` | Also scan `/proc`, `/sys`, `/dev`, autofs, … |
| `--color top\|ext\|depth` | Colour by top-level folder, by file extension, or by depth |
| `--colors truecolor\|256\|16` | Terminal colour depth (default: auto-detect) |

## Keys

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
| `m` / click the status-bar path | Select-text mode: releases the mouse so the terminal can select text (drag, double-click, triple-click). Any key returns to normal |
| `n` | Toggle nesting mode: flat ↔ header |
| `c` | Cycle colouring: top-level folder → extension → depth |
| `q` / `Ctrl-C` | Quit |

`y` => Copy <path> to clipboard.
`o` => `open <path>`

## Notes

Written by Claude, with some human highlevel product steering.

Tested on `linux` and `macos`. 

Can be used as a reusable `TreeMap` ratatui widget, a layout engine and a filesystem scanner. Ask your agent.

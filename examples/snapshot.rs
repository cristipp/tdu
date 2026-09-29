//! Render one frame headlessly and print it as text:
//! `cargo run --example snapshot -- PATH [--nesting flat|header] [--no-skip-virtual]`
use ratatui::{buffer::Buffer, layout::Rect, widgets::StatefulWidget};
use tdu::*;

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let value_of = |flag: &str| {
        let i = args.iter().position(|a| a == flag)?;
        args.get(i + 1).map(String::as_str)
    };
    let nesting = match value_of("--nesting") {
        Some("header") => Nesting::Header,
        _ => Nesting::Flat,
    };
    let path = args
        .iter()
        .enumerate()
        .find(|&(i, a)| !a.starts_with('-') && (i == 0 || args[i - 1] != "--nesting"))
        .map_or(".", |(_, s)| s.as_str());
    let opts = ScanOptions {
        skip_virtual_fs: !args.iter().any(|a| a == "--no-skip-virtual"),
        ..Default::default()
    };
    let tree = scan(path, &opts)?;
    let area = Rect::new(0, 0, 100, 30);
    let mut buf = Buffer::empty(area);
    let mut state = TreeMapState::new(tree.root());
    TreeMap::new(&tree)
        .nesting(nesting)
        .render(area, &mut buf, &mut state);
    for y in 0..area.height {
        let line: String = (0..area.width).map(|x| buf[(x, y)].symbol()).collect();
        println!("|{line}|");
    }
    let l = state.layout().unwrap();
    let root = tree.node(tree.root());
    println!(
        "total {} in {} files, {} unreadable; {} tiles, {} files ({}) not shown",
        format_size(root.size),
        root.file_count,
        tree.errors.len(),
        l.tiles.len(),
        l.hidden.files,
        format_size(l.hidden.bytes)
    );
    Ok(())
}

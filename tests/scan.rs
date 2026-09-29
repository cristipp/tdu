//! Filesystem behaviour that differs across Linux and macOS.
#![cfg(unix)]

use std::fs;
use std::io::Write;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use tdu::{
    DiskTree, NodeId, NodeKind, ScanOptions, ScanProgress, SizeMode, scan, scan_with_progress,
};

/// Self-cleaning temp directory (std only).
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!(
            "tdu-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
    fn file(&self, rel: &str, size: usize) -> PathBuf {
        let p = self.0.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::File::create(&p)
            .unwrap()
            .write_all(&vec![7u8; size])
            .unwrap();
        p
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn find(tree: &DiskTree, name: &str) -> NodeId {
    tree.ids()
        .find(|&id| tree.node(id).name == name)
        .unwrap_or_else(|| panic!("{name} not in tree"))
}

#[test]
fn sizes_aggregate_and_paths_resolve() {
    let t = TempDir::new();
    t.file("a.bin", 100);
    t.file("sub/b.bin", 50);
    t.file("sub/deeper/c.bin", 25);
    let tree = scan(&t.0, &ScanOptions::default()).unwrap();
    assert_eq!(tree.node(tree.root()).size, 175);
    assert_eq!(tree.node(tree.root()).file_count, 3);
    let c = find(&tree, "c.bin");
    assert_eq!(tree.path_of(c), t.0.join("sub/deeper/c.bin"));
    assert_eq!(tree.node(find(&tree, "sub")).size, 75);
}

#[test]
fn symlinked_root_is_followed() {
    // Like macOS's /tmp -> /private/tmp.
    let t = TempDir::new();
    t.file("real/x.bin", 300);
    symlink(t.0.join("real"), t.0.join("link")).unwrap();
    let tree = scan(t.0.join("link"), &ScanOptions::default()).unwrap();
    assert_eq!(tree.node(tree.root()).size, 300);
    assert!(
        tree.path_of(find(&tree, "x.bin"))
            .starts_with(t.0.join("link"))
    );
}

#[test]
fn symlinks_below_root_are_not_followed() {
    let t = TempDir::new();
    t.file("big.bin", 10_000);
    symlink(t.0.join("big.bin"), t.0.join("alias")).unwrap();
    symlink(&t.0, t.0.join("loop")).unwrap(); // would recurse forever if followed
    let tree = scan(&t.0, &ScanOptions::default()).unwrap();
    let alias = tree.node(find(&tree, "alias"));
    assert_eq!(alias.kind, NodeKind::Symlink);
    assert!(alias.size < 10_000);
    assert_eq!(tree.node(find(&tree, "loop")).kind, NodeKind::Symlink);
}

#[test]
fn hard_links_counted_once_by_default() {
    let t = TempDir::new();
    let a = t.file("a.bin", 1000);
    fs::hard_link(&a, t.0.join("b.bin")).unwrap();
    let tree = scan(&t.0, &ScanOptions::default()).unwrap();
    assert_eq!(tree.node(tree.root()).size, 1000);
    let opts = ScanOptions {
        dedupe_hardlinks: false,
        ..Default::default()
    };
    let tree = scan(&t.0, &opts).unwrap();
    assert_eq!(tree.node(tree.root()).size, 2000);
}

#[test]
fn allocated_size_differs_from_apparent_for_sparse_files() {
    let t = TempDir::new();
    let f = fs::File::create(t.0.join("sparse.img")).unwrap();
    f.set_len(64 << 20).unwrap(); // 64 MiB hole (sparse on APFS, ext4, xfs, btrfs, overlayfs)
    drop(f);
    let apparent = scan(&t.0, &ScanOptions::default()).unwrap();
    let alloc = scan(
        &t.0,
        &ScanOptions {
            size_mode: SizeMode::Allocated,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(apparent.node(apparent.root()).size, 64 << 20);
    assert!(alloc.node(alloc.root()).size < 1 << 20);
}

#[test]
fn unreadable_dirs_are_reported_not_fatal() {
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        return; // root ignores permissions (e.g. in Docker)
    }
    let t = TempDir::new();
    t.file("ok.bin", 10);
    t.file("locked/secret.bin", 10);
    let locked = t.0.join("locked");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let tree = scan(&t.0, &ScanOptions::default());
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    let tree = tree.unwrap();
    assert_eq!(tree.node(tree.root()).size, 10);
    assert_eq!(tree.errors.len(), 1);
    assert_eq!(tree.errors[0].0, locked);
}

#[cfg(target_os = "linux")]
#[test]
fn non_utf8_names_keep_exact_paths() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    // Linux filesystems accept arbitrary bytes; macOS APFS requires UTF-8.
    let t = TempDir::new();
    let raw = OsStr::from_bytes(b"bad\xffname.bin");
    fs::write(t.0.join(raw), [1u8; 42]).unwrap();
    let tree = scan(&t.0, &ScanOptions::default()).unwrap();
    let id = find(&tree, "bad\u{FFFD}name.bin");
    let path = tree.path_of(id);
    assert_eq!(path.file_name().unwrap(), raw);
    assert_eq!(fs::metadata(&path).unwrap().len(), 42);
}

#[test]
fn progress_counts_everything_and_ends_at_one() {
    let t = TempDir::new();
    t.file("a/a1/x.bin", 100);
    t.file("a/a2/y.bin", 200);
    t.file("a/z.bin", 300);
    t.file("b/w.bin", 400);
    t.file("top.bin", 500);
    let progress = ScanProgress::new();
    let tree = scan_with_progress(&t.0, &ScanOptions::default(), &progress).unwrap();
    let p = progress.snapshot();
    assert_eq!(p.fraction, 1.0);
    assert_eq!(p.bytes, 1500);
    assert_eq!(p.bytes, tree.node(tree.root()).size);
    // Entries: a, a1, a2, b, top.bin, x, y, z, w.
    assert_eq!(p.entries, 9);
    assert_eq!(p.dirs, 5); // root, a, a1, a2, b
    assert_eq!(p.errors, 0);
    assert!(p.current.starts_with(&t.0));
}

#[test]
fn cancelled_scan_stops_with_interrupted() {
    let t = TempDir::new();
    t.file("a/x.bin", 1);
    let progress = ScanProgress::new();
    progress.cancel();
    let err = scan_with_progress(&t.0, &ScanOptions::default(), &progress).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::Interrupted);
}

#[test]
fn progress_can_be_watched_from_another_thread() {
    let t = TempDir::new();
    for i in 0..50 {
        t.file(&format!("d{i}/f.bin"), 10);
    }
    let progress = std::sync::Arc::new(ScanProgress::new());
    let root = t.0.clone();
    let worker = {
        let progress = progress.clone();
        std::thread::spawn(move || scan_with_progress(&root, &ScanOptions::default(), &progress))
    };
    // Estimates observed along the way never go down.
    let mut last = 0.0;
    while !worker.is_finished() {
        let f = progress.snapshot().fraction;
        assert!(f >= last && f <= 1.0, "{last} -> {f}");
        last = f;
    }
    let tree = worker.join().unwrap().unwrap();
    assert_eq!(tree.node(tree.root()).size, 500);
    assert_eq!(progress.snapshot().fraction, 1.0);
}

#[test]
fn relative_roots_become_absolute() {
    let tree = scan("src", &ScanOptions::default()).unwrap();
    assert!(tree.root_path().is_absolute());
    assert!(tree.root_path().ends_with("src"));
    let lib = find(&tree, "lib.rs");
    assert!(tree.path_of(lib).is_absolute());
    assert!(tree.path_of(lib).is_file());
}

#[test]
fn scanning_a_single_file_works() {
    let t = TempDir::new();
    let f = t.file("only.bin", 123);
    let tree = scan(&f, &ScanOptions::default()).unwrap();
    assert_eq!(tree.node(tree.root()).size, 123);
    assert_eq!(tree.path_of(find(&tree, "only.bin")), f);
    let _: &Path = tree.root_path();
}

//! The data model: an immutable, arena-allocated tree of file sizes.
//!
//! A [`DiskTree`] can be produced by [`scan`]ning the filesystem or built by
//! hand with [`DiskTreeBuilder`] (useful for tests, archives, remote data, ...).
//! The rendering layer only ever sees a `DiskTree`, so it doesn't care where
//! the sizes came from.

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Index of a node inside a [`DiskTree`]. Cheap to copy, only meaningful for
/// the tree that produced it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(u32);

impl NodeId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeKind {
    File,
    Dir,
    Symlink,
    /// Sockets, fifos, devices, ...
    Other,
}

#[derive(Clone, Debug)]
pub struct Node {
    /// Display name (lossily converted to UTF-8).
    pub name: String,
    /// The exact on-disk name, kept only when it isn't valid UTF-8 (possible
    /// on Linux). [`DiskTree::path_of`] uses it so paths stay correct.
    pub raw_name: Option<OsString>,
    pub kind: NodeKind,
    /// Bytes. For directories: the sum over all descendants.
    pub size: u64,
    /// Number of non-directory descendants (1 for a file).
    pub file_count: u64,
    pub parent: Option<NodeId>,
    /// Sorted by `size`, largest first (ties broken by name).
    pub children: Vec<NodeId>,
}

impl Node {
    pub fn is_dir(&self) -> bool {
        self.kind == NodeKind::Dir
    }
}

static NEXT_TREE_ID: AtomicU64 = AtomicU64::new(1);

/// Immutable tree of sizes. Node 0 is the root.
#[derive(Debug)]
pub struct DiskTree {
    nodes: Vec<Node>,
    root_path: PathBuf,
    /// Unique per finalized tree; lets widget state detect a swapped tree.
    id: u64,
    /// Entries that couldn't be read while scanning.
    pub errors: Vec<(PathBuf, io::Error)>,
}

impl DiskTree {
    pub fn root(&self) -> NodeId {
        NodeId(0)
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id.index()]
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn root_path(&self) -> &Path {
        &self.root_path
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    /// All node ids; parents always come before their children.
    pub fn ids(&self) -> impl Iterator<Item = NodeId> + use<> {
        (0..self.nodes.len() as u32).map(NodeId)
    }

    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.node(id).parent
    }

    /// `id`, its parent, grandparent, ... up to the root.
    pub fn ancestors(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        std::iter::successors(Some(id), move |&n| self.parent(n))
    }

    pub fn is_ancestor_or_self(&self, ancestor: NodeId, id: NodeId) -> bool {
        self.ancestors(id).any(|n| n == ancestor)
    }

    /// The child of `ancestor` on the path down to `id`, if `id` is strictly
    /// below `ancestor`.
    pub fn child_toward(&self, ancestor: NodeId, id: NodeId) -> Option<NodeId> {
        let mut prev = None;
        for n in self.ancestors(id) {
            if n == ancestor {
                return prev;
            }
            prev = Some(n);
        }
        None
    }

    /// Full filesystem path (root path joined with the node's components).
    pub fn path_of(&self, id: NodeId) -> PathBuf {
        let mut names: Vec<&std::ffi::OsStr> = self
            .ancestors(id)
            .take_while(|&n| n != self.root())
            .map(|n| {
                let node = self.node(n);
                node.raw_name.as_deref().unwrap_or(node.name.as_ref())
            })
            .collect();
        names.reverse();
        let mut p = self.root_path.clone();
        p.extend(names);
        p
    }
}

/// Builds a [`DiskTree`] from arbitrary data. Directory sizes are computed
/// and children are sorted in [`DiskTreeBuilder::finish`].
pub struct DiskTreeBuilder {
    nodes: Vec<Node>,
    root_path: PathBuf,
    errors: Vec<(PathBuf, io::Error)>,
}

impl DiskTreeBuilder {
    pub fn new(root_path: impl Into<PathBuf>) -> Self {
        let root_path = root_path.into();
        let name = root_path.display().to_string();
        DiskTreeBuilder {
            nodes: vec![Node {
                name,
                raw_name: None,
                kind: NodeKind::Dir,
                size: 0,
                file_count: 0,
                parent: None,
                children: Vec::new(),
            }],
            root_path,
            errors: Vec::new(),
        }
    }

    pub fn root(&self) -> NodeId {
        NodeId(0)
    }

    pub fn add_dir(&mut self, parent: NodeId, name: impl Into<String>) -> NodeId {
        self.push(parent, name.into(), None, NodeKind::Dir, 0)
    }

    pub fn add_file(&mut self, parent: NodeId, name: impl Into<String>, size: u64) -> NodeId {
        self.push(parent, name.into(), None, NodeKind::File, size)
    }

    pub fn add_entry(
        &mut self,
        parent: NodeId,
        name: impl Into<String>,
        kind: NodeKind,
        size: u64,
    ) -> NodeId {
        self.push(parent, name.into(), None, kind, size)
    }

    /// Like [`add_entry`](Self::add_entry) but takes an OS name, which may
    /// not be valid UTF-8.
    pub fn add_entry_os(
        &mut self,
        parent: NodeId,
        name: OsString,
        kind: NodeKind,
        size: u64,
    ) -> NodeId {
        match name.into_string() {
            Ok(s) => self.push(parent, s, None, kind, size),
            Err(raw) => {
                let lossy = raw.to_string_lossy().into_owned();
                self.push(parent, lossy, Some(raw), kind, size)
            }
        }
    }

    pub fn add_error(&mut self, path: PathBuf, err: io::Error) {
        self.errors.push((path, err));
    }

    fn push(
        &mut self,
        parent: NodeId,
        name: String,
        raw_name: Option<OsString>,
        kind: NodeKind,
        size: u64,
    ) -> NodeId {
        assert!(
            self.nodes[parent.index()].kind == NodeKind::Dir,
            "parent must be a directory"
        );
        let id = NodeId(u32::try_from(self.nodes.len()).expect("too many nodes"));
        let (size, file_count) = if kind == NodeKind::Dir {
            (0, 0)
        } else {
            (size, 1)
        };
        self.nodes.push(Node {
            name,
            raw_name,
            kind,
            size,
            file_count,
            parent: Some(parent),
            children: Vec::new(),
        });
        self.nodes[parent.index()].children.push(id);
        id
    }

    pub fn finish(mut self) -> DiskTree {
        // Children always have larger ids than their parent, so a single
        // reverse pass aggregates sizes bottom-up.
        for i in (1..self.nodes.len()).rev() {
            let (size, count) = (self.nodes[i].size, self.nodes[i].file_count);
            let p = self.nodes[i].parent.expect("non-root has parent").index();
            self.nodes[p].size += size;
            self.nodes[p].file_count += count;
        }
        for i in 0..self.nodes.len() {
            let mut children = std::mem::take(&mut self.nodes[i].children);
            children.sort_by(|a, b| {
                let (na, nb) = (&self.nodes[a.index()], &self.nodes[b.index()]);
                nb.size.cmp(&na.size).then_with(|| na.name.cmp(&nb.name))
            });
            self.nodes[i].children = children;
        }
        DiskTree {
            nodes: self.nodes,
            root_path: self.root_path,
            id: NEXT_TREE_ID.fetch_add(1, Ordering::Relaxed),
            errors: self.errors,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SizeMode {
    /// File length in bytes (what `ls -l` shows).
    #[default]
    Apparent,
    /// Bytes actually allocated on disk (what `du` shows): `st_blocks * 512`
    /// on Linux and macOS. Falls back to `Apparent` on other platforms.
    Allocated,
}

#[derive(Clone, Debug)]
pub struct ScanOptions {
    pub size_mode: SizeMode,
    /// Don't descend into directories on other filesystems (`du -x`).
    pub one_file_system: bool,
    /// Count each hard-linked inode once, like `du` (default: true).
    pub dedupe_hardlinks: bool,
    /// Skip kernel/virtual filesystems whose sizes aren't disk usage
    /// (default: true). Linux: `proc`, `sysfs`, `cgroup`, `debugfs`, ...
    /// (`/proc/kcore` alone claims ~128 TiB). macOS: `devfs`, `autofs`
    /// (which can hang on network automounts), `fdesc`. The scan root itself is
    /// never skipped.
    pub skip_virtual_fs: bool,
}

impl Default for ScanOptions {
    fn default() -> Self {
        ScanOptions {
            size_mode: SizeMode::Apparent,
            one_file_system: false,
            dedupe_hardlinks: true,
            skip_virtual_fs: true,
        }
    }
}

/// Live progress of a [`scan_with_progress`], safe to read from another thread
/// (e.g. a UI) while the scan runs. The scanner only does relaxed atomic adds
/// per entry, plus one uncontended lock per directory for the current path.
#[derive(Debug, Default)]
pub struct ScanProgress {
    entries: AtomicU64,
    bytes: AtomicU64,
    dirs: AtomicU64,
    errors: AtomicU64,
    /// Estimated completed fraction, as `f64` bits.
    fraction: AtomicU64,
    current: Mutex<PathBuf>,
    cancel: AtomicBool,
}

/// A consistent-enough copy of [`ScanProgress`] for display.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProgressSnapshot {
    /// Entries seen so far (files, dirs, symlinks, ...).
    pub entries: u64,
    /// Bytes counted so far.
    pub bytes: u64,
    /// Directories read so far.
    pub dirs: u64,
    /// Unreadable entries so far.
    pub errors: u64,
    /// Estimated fraction done, in `0.0..=1.0`. See [`scan_with_progress`].
    pub fraction: f64,
    /// Directory being read right now.
    pub current: PathBuf,
}

impl ScanProgress {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn snapshot(&self) -> ProgressSnapshot {
        ProgressSnapshot {
            entries: self.entries.load(Ordering::Relaxed),
            bytes: self.bytes.load(Ordering::Relaxed),
            dirs: self.dirs.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
            fraction: f64::from_bits(self.fraction.load(Ordering::Relaxed)),
            current: self.current.lock().map(|p| p.clone()).unwrap_or_default(),
        }
    }

    /// Ask the scan to stop. It returns `ErrorKind::Interrupted` soon after.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    fn add_done(&self, w: f64) {
        let f = f64::from_bits(self.fraction.load(Ordering::Relaxed));
        // Only the scanning thread writes, so load-then-store is fine.
        self.fraction
            .store((f + w).min(1.0).to_bits(), Ordering::Relaxed);
    }
}

/// Walk `path` and build a [`DiskTree`].
///
/// * The root is made absolute, so [`DiskTree::path_of`] returns absolute
///   paths.
/// * The root is resolved through symlinks (on macOS `/tmp`, `/var` and `/etc`
///   are symlinks into `/private`); nothing below it is followed.
/// * Every directory is visited once per `(device, inode)`. This stops macOS
///   firmlinks (`/Users` == `/System/Volumes/Data/Users`) from being counted
///   twice and stops Linux bind mounts from looping forever.
/// * Unreadable entries are recorded in [`DiskTree::errors`] rather than
///   aborting the scan.
pub fn scan(path: impl AsRef<Path>, opts: &ScanOptions) -> io::Result<DiskTree> {
    scan_with_progress(path, opts, &ScanProgress::new())
}

/// [`scan`], reporting into `progress` as it goes and stopping with
/// `ErrorKind::Interrupted` if [`ScanProgress::cancel`] is called.
///
/// The total size isn't known until the scan finishes, so
/// [`ProgressSnapshot::fraction`] is a structural *estimate*. The root
/// directory is worth 1.0. Each directory's share is split evenly among its
/// subdirectories, and a directory with none counts as done once it has been
/// read. The estimate only ever grows and reaches exactly 1.0 at the end, but
/// it can race ahead or stall when one subdirectory holds most of the files.
pub fn scan_with_progress(
    path: impl AsRef<Path>,
    opts: &ScanOptions,
    progress: &ScanProgress,
) -> io::Result<DiskTree> {
    // Absolute (but symlinks kept as written), so `path_of` yields paths that
    // can be copied or opened from anywhere.
    let path = &std::path::absolute(path.as_ref())?;
    let meta = fs::metadata(path)?; // follow a symlinked root
    let mut b = DiskTreeBuilder::new(path);
    if !meta.is_dir() {
        let name = path
            .file_name()
            .map_or_else(|| path.as_os_str().to_owned(), |n| n.to_owned());
        b.root_path = path.parent().map(Path::to_path_buf).unwrap_or_default();
        let root = b.root();
        let size = size_of(&meta, opts.size_mode);
        b.add_entry_os(root, name, kind_of(&meta), size);
        progress.entries.fetch_add(1, Ordering::Relaxed);
        progress.bytes.fetch_add(size, Ordering::Relaxed);
        progress.add_done(1.0);
        return Ok(b.finish());
    }

    let cancelled = || io::Error::new(io::ErrorKind::Interrupted, "scan cancelled");
    let root_dev = sys::dev(&meta);
    let mut seen_inodes: HashSet<(u64, u64)> = HashSet::new();
    let mut seen_dirs: HashSet<(u64, u64)> = HashSet::new();
    seen_dirs.extend(sys::inode_key(&meta));
    // (path, node, device, share of the progress estimate)
    let mut stack = vec![(path.to_path_buf(), b.root(), sys::dev(&meta), 1.0f64)];
    while let Some((dir, dir_id, dir_dev, weight)) = stack.pop() {
        if progress.is_cancelled() {
            return Err(cancelled());
        }
        if let Ok(mut cur) = progress.current.lock() {
            cur.clone_from(&dir);
        }
        progress.dirs.fetch_add(1, Ordering::Relaxed);
        let first_child = stack.len();
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) => {
                b.add_error(dir, e);
                progress.errors.fetch_add(1, Ordering::Relaxed);
                progress.add_done(weight);
                continue;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    b.add_error(dir.clone(), e);
                    progress.errors.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
            };
            let n = progress.entries.fetch_add(1, Ordering::Relaxed);
            if n.is_multiple_of(4096) && progress.is_cancelled() {
                return Err(cancelled()); // huge directories: don't wait for the end
            }
            let child_path = entry.path();
            // DirEntry::metadata doesn't follow symlinks.
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(e) => {
                    b.add_error(child_path, e);
                    progress.errors.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
            };
            let kind = kind_of(&meta);
            if kind == NodeKind::Dir {
                let dev = sys::dev(&meta);
                if dev != dir_dev {
                    // Crossing a mount point.
                    if opts.one_file_system && dev != root_dev {
                        continue;
                    }
                    if opts.skip_virtual_fs && sys::is_virtual_fs(&child_path) {
                        continue;
                    }
                }
                if let Some(key) = sys::inode_key(&meta)
                    && !seen_dirs.insert(key)
                {
                    continue;
                }
                let id = b.add_entry_os(dir_id, entry.file_name(), NodeKind::Dir, 0);
                stack.push((child_path, id, dev, 0.0)); // share assigned below
            } else {
                if opts.dedupe_hardlinks
                    && let Some(key) = sys::hardlink_key(&meta)
                    && !seen_inodes.insert(key)
                {
                    continue;
                }
                let size = size_of(&meta, opts.size_mode);
                progress.bytes.fetch_add(size, Ordering::Relaxed);
                b.add_entry_os(dir_id, entry.file_name(), kind, size);
            }
        }
        // Split this directory's share among its subdirectories, or bank it.
        let subdirs = stack.len() - first_child;
        if subdirs == 0 {
            progress.add_done(weight);
        } else {
            let share = weight / subdirs as f64;
            for item in &mut stack[first_child..] {
                item.3 = share;
            }
        }
    }
    // Float rounding could leave the sum a hair under 1.0.
    progress.fraction.store(1.0f64.to_bits(), Ordering::Relaxed);
    Ok(b.finish())
}

fn kind_of(meta: &fs::Metadata) -> NodeKind {
    let ft = meta.file_type();
    if ft.is_dir() {
        NodeKind::Dir
    } else if ft.is_file() {
        NodeKind::File
    } else if ft.is_symlink() {
        NodeKind::Symlink
    } else {
        NodeKind::Other
    }
}

fn size_of(meta: &fs::Metadata, mode: SizeMode) -> u64 {
    match mode {
        SizeMode::Apparent => meta.len(),
        SizeMode::Allocated => sys::allocated(meta).unwrap_or(meta.len()),
    }
}

#[cfg(unix)]
mod sys {
    use std::fs::Metadata;
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;

    pub fn dev(m: &Metadata) -> u64 {
        m.dev()
    }
    pub fn allocated(m: &Metadata) -> Option<u64> {
        // st_blocks is in 512-byte units on both Linux and macOS.
        Some(m.blocks() * 512)
    }
    pub fn inode_key(m: &Metadata) -> Option<(u64, u64)> {
        Some((m.dev(), m.ino()))
    }
    pub fn hardlink_key(m: &Metadata) -> Option<(u64, u64)> {
        (m.nlink() > 1).then(|| (m.dev(), m.ino()))
    }

    fn statfs(path: &Path) -> Option<libc::statfs> {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
        let mut st = std::mem::MaybeUninit::<libc::statfs>::zeroed();
        // SAFETY: `c` is a valid NUL-terminated string and `st` is a valid
        // out-pointer; statfs fully initializes it on success.
        let rc = unsafe { libc::statfs(c.as_ptr(), st.as_mut_ptr()) };
        (rc == 0).then(|| unsafe { st.assume_init() })
    }

    /// Kernel pseudo-filesystems whose "sizes" aren't disk usage.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub fn is_virtual_fs(path: &Path) -> bool {
        const VIRTUAL: &[u32] = &[
            0x9fa0,      // proc
            0x6265_6572, // sysfs
            0x1cd1,      // devpts
            0x6462_6720, // debugfs
            0x7472_6163, // tracefs
            0x7363_6673, // securityfs
            0x0027_e0eb, // cgroup
            0x6367_7270, // cgroup2
            0x6165_676c, // pstore
            0xcafe_4a11, // bpf
            0x6265_6570, // configfs
            0x6573_5543, // fusectl
            0x1980_0202, // mqueue
            0x9584_58f6, // hugetlbfs
            0x4249_4e4d, // binfmt_misc
            0xde5e_81e4, // efivarfs
            0xf97c_ff8c, // selinuxfs
            0x6e73_6673, // nsfs
            0x0187,      // autofs
        ];
        // f_type's integer type varies by libc/arch; compare the low 32 bits.
        statfs(path).is_some_and(|st| VIRTUAL.contains(&(st.f_type as u32)))
    }

    #[cfg(target_os = "macos")]
    pub fn is_virtual_fs(path: &Path) -> bool {
        const VIRTUAL: &[&[u8]] = &[b"devfs", b"autofs", b"fdesc"];
        statfs(path).is_some_and(|st| {
            let name: Vec<u8> = st
                .f_fstypename
                .iter()
                .take_while(|&&c| c != 0)
                .map(|&c| c as u8)
                .collect();
            VIRTUAL.contains(&name.as_slice())
        })
    }

    #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
    pub fn is_virtual_fs(_: &Path) -> bool {
        false
    }
}

#[cfg(not(unix))]
mod sys {
    use std::fs::Metadata;
    use std::path::Path;

    pub fn dev(_: &Metadata) -> u64 {
        0
    }
    pub fn allocated(_: &Metadata) -> Option<u64> {
        None
    }
    pub fn inode_key(_: &Metadata) -> Option<(u64, u64)> {
        None
    }
    pub fn hardlink_key(_: &Metadata) -> Option<(u64, u64)> {
        None
    }
    pub fn is_virtual_fs(_: &Path) -> bool {
        false
    }
}

/// `1536` -> `"1.5 KiB"`.
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 7] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if v < 10.0 {
        format!("{v:.1} {}", UNITS[u])
    } else {
        format!("{v:.0} {}", UNITS[u])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn detects_linux_virtual_filesystems() {
        assert!(sys::is_virtual_fs(Path::new("/proc")));
        assert!(sys::is_virtual_fs(Path::new("/sys")));
        assert!(!sys::is_virtual_fs(Path::new("/")));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn detects_macos_virtual_filesystems() {
        assert!(sys::is_virtual_fs(Path::new("/dev")));
        assert!(!sys::is_virtual_fs(Path::new("/")));
    }

    #[test]
    fn format_sizes() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1536), "1.5 KiB");
        assert_eq!(format_size(755 << 20), "755 MiB");
    }
}

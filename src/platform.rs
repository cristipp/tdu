//! Desktop integration for the `ydu` binary: copying to the clipboard and
//! opening paths with the default application, on macOS and Linux.
//!
//! Commands run in the background with stdio detached, so they can never block
//! or draw over the TUI. Their outcome comes back as a [`Notice`] on a channel.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyModifiers};

/// A status-bar message produced by a background action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub text: String,
    pub error: bool,
}

impl Notice {
    pub fn ok(text: impl Into<String>) -> Self {
        Notice {
            text: text.into(),
            error: false,
        }
    }
    fn err(text: impl Into<String>) -> Self {
        Notice {
            text: text.into(),
            error: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Mac,
    Linux,
    Other,
}

impl Os {
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Os::Mac
        } else if cfg!(target_os = "linux") {
            Os::Linux
        } else {
            Os::Other
        }
    }
}

/// The session facts that decide which tools to use (read from the
/// environment, and injectable for tests).
#[derive(Clone, Debug, Default)]
pub struct Env {
    pub ssh: bool,
    pub wayland: bool,
    pub x11: bool,
}

impl Env {
    pub fn current() -> Self {
        let set = |k: &str| std::env::var_os(k).is_some_and(|v| !v.is_empty());
        Env {
            ssh: set("SSH_CONNECTION") || set("SSH_TTY"),
            wayland: set("WAYLAND_DISPLAY"),
            x11: set("DISPLAY"),
        }
    }
}

/// A program plus its fixed arguments.
pub type Cmd = (&'static str, &'static [&'static str]);

/// Is this key press the "copy path" shortcut?
///
/// * macOS: **Cmd-C** (`SUPER`), the platform's copy key.
/// * Linux: **Ctrl-Shift-C**, the terminal-world copy key (Ctrl-C is
///   interrupt).
/// * Anywhere: plain **`y`** ("yank"). Terminals pass it through unchanged,
///   whereas they usually keep Cmd-C / Ctrl-Shift-C for their own copy
///   command, and legacy terminals can't report those modifiers at all.
///
/// Cmd-C and Ctrl-Shift-C only reach the app when the terminal speaks the
/// kitty keyboard protocol (kitty, WezTerm, Ghostty, foot, Alacritty, iTerm2
/// with "CSI u" reporting) *and* isn't binding the key itself.
pub fn is_copy_key(os: Os, code: KeyCode, mods: KeyModifiers) -> bool {
    let c = matches!(code, KeyCode::Char('c') | KeyCode::Char('C'));
    match os {
        _ if code == KeyCode::Char('y') && mods.is_empty() => true,
        Os::Mac => c && mods.contains(KeyModifiers::SUPER),
        Os::Linux => c && mods.contains(KeyModifiers::CONTROL | KeyModifiers::SHIFT),
        Os::Other => false,
    }
}

/// How to show the copy shortcut in the help line. `enhanced` says whether
/// the terminal reports modifier keys (kitty keyboard protocol).
pub fn copy_key_label(os: Os, enhanced: bool) -> &'static str {
    match (os, enhanced) {
        (Os::Mac, true) => "⌘C/y",
        (Os::Linux, true) => "^⇧C/y",
        _ => "y",
    }
}

/// Clipboard programs to try, in order. Empty over SSH: a tool there would
/// fill the *remote* machine's clipboard, so we use OSC 52 instead.
pub fn clipboard_commands(os: Os, env: &Env) -> Vec<Cmd> {
    if env.ssh {
        return vec![];
    }
    match os {
        Os::Mac => vec![("pbcopy", &[])],
        Os::Linux => {
            let mut v: Vec<Cmd> = Vec::new();
            if env.wayland {
                v.push(("wl-copy", &[]));
            }
            if env.x11 {
                v.push(("xclip", &["-selection", "clipboard"]));
                v.push(("xsel", &["--clipboard", "--input"]));
            }
            v
        }
        Os::Other => vec![],
    }
}

/// "Open with the default application" programs to try, in order.
pub fn open_commands(os: Os) -> Vec<Cmd> {
    match os {
        Os::Mac => vec![("open", &[])],
        Os::Linux => vec![("xdg-open", &[]), ("gio", &["open"])],
        Os::Other => vec![],
    }
}

/// Find an executable on `$PATH`.
fn which(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|p| is_executable(p))
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

fn first_available(cmds: &[Cmd]) -> Option<(Cmd, PathBuf)> {
    cmds.iter().find_map(|&c| which(c.0).map(|p| (c, p)))
}

/// Copy `text` to the clipboard.
///
/// Locally, this pipes into the native tool (`pbcopy`; `wl-copy`, `xclip` or
/// `xsel`) and reports the result on `tx` once the tool exits. Over SSH, or
/// when no tool is installed, it writes an OSC 52 escape sequence to
/// `terminal`, which asks the terminal emulator to set the clipboard of the
/// machine you're sitting at. Most modern terminals support this (iTerm2,
/// kitty, WezTerm, Alacritty, foot, GNOME Terminal 3.52+); inside tmux it
/// needs `set -g set-clipboard on`.
pub fn copy(text: &str, terminal: &mut impl Write, tx: &Sender<Notice>) {
    let label = shorten(text);
    let cmds = clipboard_commands(Os::current(), &Env::current());
    let Some(((name, args), program)) = first_available(&cmds) else {
        let result = terminal
            .write_all(osc52(text).as_bytes())
            .and_then(|_| terminal.flush());
        let _ = tx.send(match result {
            Ok(()) => Notice::ok(format!("Copied {label} (via terminal, OSC 52)")),
            Err(e) => Notice::err(format!("Copy failed: {e}")),
        });
        return;
    };
    let spawned = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(Notice::err(format!("Copy failed: {name}: {e}")));
            return;
        }
    };
    let text = text.to_owned();
    let tx = tx.clone();
    // Tools like xclip keep running to serve the selection, so wait off-thread.
    std::thread::spawn(move || {
        let wrote = child
            .stdin
            .take()
            .map(|mut stdin| stdin.write_all(text.as_bytes()))
            .unwrap_or(Ok(()));
        let status = child.wait();
        let _ = tx.send(match (wrote, status) {
            (Ok(()), Ok(s)) if s.success() => Notice::ok(format!("Copied {label}")),
            (Err(e), _) | (_, Err(e)) => Notice::err(format!("Copy failed: {name}: {e}")),
            (_, Ok(s)) => Notice::err(format!("Copy failed: {name} exited with {s}")),
        });
    });
}

/// Open `path` with the default application (`open` on macOS, `xdg-open` or
/// `gio open` on Linux), without waiting for it.
pub fn open(path: &Path, tx: &Sender<Notice>) {
    let label = shorten(&path.to_string_lossy());
    let Some(((name, args), program)) = first_available(&open_commands(Os::current())) else {
        let hint = match Os::current() {
            Os::Linux => " (install xdg-utils)",
            _ => "",
        };
        let _ = tx.send(Notice::err(format!("Can't open: no opener found{hint}")));
        return;
    };
    let spawned = Command::new(program)
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match spawned {
        Ok(mut child) => {
            let tx = tx.clone();
            // Reap the child and report failures such as "no application
            // knows how to open this file".
            std::thread::spawn(move || {
                let _ = tx.send(match child.wait() {
                    Ok(s) if s.success() => Notice::ok(format!("Opened {label}")),
                    Ok(s) => Notice::err(format!("{name} {label} failed ({s})")),
                    Err(e) => Notice::err(format!("{name} failed: {e}")),
                });
            });
        }
        Err(e) => {
            let _ = tx.send(Notice::err(format!("{name} failed: {e}")));
        }
    }
}

/// OSC 52 "set clipboard" sequence.
pub fn osc52(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64(text.as_bytes()))
}

fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Keep status messages to one line: `…/parent/file` for long paths.
fn shorten(text: &str) -> String {
    const MAX: usize = 60;
    let n = text.chars().count();
    if n <= MAX {
        text.to_string()
    } else {
        let tail: String = text.chars().skip(n - (MAX - 1)).collect();
        format!("…{tail}")
    }
}

/// Detects double-clicks: crossterm reports each press separately.
#[derive(Debug)]
pub struct ClickTracker<T> {
    last: Option<(Instant, T)>,
}

// Manual impl: derive would needlessly require `T: Default`.
impl<T> Default for ClickTracker<T> {
    fn default() -> Self {
        ClickTracker { last: None }
    }
}

impl<T: PartialEq + Copy> ClickTracker<T> {
    /// Max gap between the two presses (macOS's default is about 500 ms).
    pub const INTERVAL: Duration = Duration::from_millis(500);

    /// Record a press on `target`. Returns true if it completes a
    /// double-click (a second press on the same target within
    /// [`INTERVAL`](Self::INTERVAL)). A third press starts over.
    pub fn press(&mut self, now: Instant, target: T) -> bool {
        match self.last {
            Some((t, prev)) if prev == target && now.duration_since(t) <= Self::INTERVAL => {
                self.last = None;
                true
            }
            _ => {
                self.last = Some((now, target));
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc4648_vectors() {
        let cases = [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ];
        for (input, want) in cases {
            assert_eq!(base64(input.as_bytes()), want, "{input:?}");
        }
        assert_eq!(osc52("/tmp/a b"), "\x1b]52;c;L3RtcC9hIGI=\x07");
    }

    #[test]
    fn picks_clipboard_tools_per_platform() {
        let local = Env::default();
        assert_eq!(clipboard_commands(Os::Mac, &local)[0].0, "pbcopy");
        let wayland = Env {
            wayland: true,
            x11: true,
            ..local.clone()
        };
        let names: Vec<_> = clipboard_commands(Os::Linux, &wayland)
            .iter()
            .map(|c| c.0)
            .collect();
        assert_eq!(names, ["wl-copy", "xclip", "xsel"]);
        let x11 = Env {
            x11: true,
            ..local.clone()
        };
        assert_eq!(clipboard_commands(Os::Linux, &x11)[0].0, "xclip");
        // Headless Linux, or any SSH session: fall back to OSC 52.
        assert!(clipboard_commands(Os::Linux, &local).is_empty());
        let ssh = Env {
            ssh: true,
            x11: true,
            ..local
        };
        assert!(clipboard_commands(Os::Mac, &ssh).is_empty());
        assert!(clipboard_commands(Os::Linux, &ssh).is_empty());
    }

    #[test]
    fn picks_openers_per_platform() {
        assert_eq!(open_commands(Os::Mac), vec![("open", &[][..])]);
        let linux: Vec<_> = open_commands(Os::Linux).iter().map(|c| c.0).collect();
        assert_eq!(linux, ["xdg-open", "gio"]);
    }

    #[test]
    fn copy_key_per_platform() {
        let (none, ctrl, shift, sup) = (
            KeyModifiers::NONE,
            KeyModifiers::CONTROL,
            KeyModifiers::SHIFT,
            KeyModifiers::SUPER,
        );
        let c = KeyCode::Char('c');
        let upper_c = KeyCode::Char('C');
        let y = KeyCode::Char('y');
        // macOS: Cmd-C (some terminals report the shifted/upper form).
        assert!(is_copy_key(Os::Mac, c, sup));
        assert!(is_copy_key(Os::Mac, upper_c, sup | shift));
        assert!(!is_copy_key(Os::Mac, c, ctrl));
        assert!(!is_copy_key(Os::Mac, c, none)); // plain c = colours
        // Linux: Ctrl-Shift-C, but not Ctrl-C (that's quit).
        assert!(is_copy_key(Os::Linux, c, ctrl | shift));
        assert!(is_copy_key(Os::Linux, upper_c, ctrl | shift));
        assert!(!is_copy_key(Os::Linux, c, ctrl));
        assert!(!is_copy_key(Os::Linux, c, sup));
        // `y` everywhere, but not with modifiers.
        for os in [Os::Mac, Os::Linux, Os::Other] {
            assert!(is_copy_key(os, y, none));
            assert!(!is_copy_key(os, y, ctrl));
        }
        assert_eq!(copy_key_label(Os::Mac, true), "⌘C/y");
        assert_eq!(copy_key_label(Os::Linux, true), "^⇧C/y");
        assert_eq!(copy_key_label(Os::Mac, false), "y");
    }

    #[test]
    fn detects_double_clicks() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut c = ClickTracker::default();
        assert!(!c.press(ms(0), 1));
        assert!(c.press(ms(300), 1)); // double-click
        assert!(!c.press(ms(400), 1)); // third press starts over
        assert!(!c.press(ms(1000), 1)); // too slow
        assert!(!c.press(ms(1100), 2)); // different target
        assert!(c.press(ms(1200), 2));
    }

    #[test]
    fn which_finds_sh() {
        assert!(which("sh").is_some());
        assert!(which("definitely-not-a-real-program-ydu").is_none());
    }

    #[test]
    fn shortens_long_paths_from_the_left() {
        let long = format!("/{}", "a/".repeat(50));
        let s = shorten(&long);
        assert_eq!(s.chars().count(), 60);
        assert!(s.starts_with('…') && s.ends_with("a/"));
        assert_eq!(shorten("/short"), "/short");
    }

    /// End-to-end through real process spawning, with fake tools on `$PATH`
    /// that record their arguments and stdin. These tests change process-wide
    /// environment variables, so they're `#[ignore]`d. Run them with
    /// `cargo test -- --ignored --test-threads=1`.
    mod spawn {
        use super::super::*;
        use std::fs;
        use std::os::unix::fs::PermissionsExt;
        use std::sync::mpsc;

        struct FakeTools {
            dir: PathBuf,
            old_path: std::ffi::OsString,
        }

        impl FakeTools {
            /// Install fake `pbcopy`, `wl-copy`, `xclip`, `open` and `xdg-open`
            /// that append "<name> <args>|<stdin>" to `log`.
            fn install() -> Self {
                let dir = std::env::temp_dir().join(format!("ydu-fake-{}", std::process::id()));
                fs::create_dir_all(&dir).unwrap();
                let log = dir.join("log");
                for name in ["pbcopy", "wl-copy", "xclip", "open", "xdg-open"] {
                    let script = format!(
                        "#!/bin/sh\nin=$(cat)\nprintf '%s %s|%s\\n' {name} \"$*\" \"$in\" >> '{}'\n",
                        log.display()
                    );
                    let p = dir.join(name);
                    fs::write(&p, script).unwrap();
                    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
                }
                let old_path = std::env::var_os("PATH").unwrap_or_default();
                // SAFETY: these tests are ignored by default and meant to run
                // with --test-threads=1, so nothing reads the env concurrently.
                unsafe {
                    std::env::set_var("PATH", format!("{}:/usr/bin:/bin", dir.display()));
                    std::env::remove_var("SSH_CONNECTION");
                    std::env::remove_var("SSH_TTY");
                    std::env::remove_var("WAYLAND_DISPLAY");
                    std::env::set_var("DISPLAY", ":99");
                }
                FakeTools { dir, old_path }
            }

            fn log(&self) -> String {
                fs::read_to_string(self.dir.join("log")).unwrap_or_default()
            }
        }

        impl Drop for FakeTools {
            fn drop(&mut self) {
                // SAFETY: see `install`.
                unsafe { std::env::set_var("PATH", &self.old_path) };
                let _ = fs::remove_dir_all(&self.dir);
            }
        }

        fn wait(rx: &mpsc::Receiver<Notice>) -> Notice {
            rx.recv_timeout(Duration::from_secs(5)).expect("a notice")
        }

        #[test]
        #[ignore]
        fn copy_pipes_path_into_native_tool() {
            let fake = FakeTools::install();
            let (tx, rx) = mpsc::channel();
            let mut term = Vec::new();
            copy("/data/My Files/a.txt", &mut term, &tx);
            let n = wait(&rx);
            assert!(!n.error, "{n:?}");
            assert_eq!(n.text, "Copied /data/My Files/a.txt");
            assert!(term.is_empty(), "no OSC 52 when a native tool exists");
            let want = if cfg!(target_os = "macos") {
                "pbcopy |/data/My Files/a.txt\n"
            } else {
                "xclip -selection clipboard|/data/My Files/a.txt\n"
            };
            assert_eq!(fake.log(), want);
        }

        #[test]
        #[ignore]
        fn copy_over_ssh_uses_osc52() {
            let fake = FakeTools::install();
            // SAFETY: see `FakeTools::install`.
            unsafe { std::env::set_var("SSH_CONNECTION", "1.2.3.4 5 6.7.8.9 22") };
            let (tx, rx) = mpsc::channel();
            let mut term = Vec::new();
            copy("/srv/x", &mut term, &tx);
            unsafe { std::env::remove_var("SSH_CONNECTION") };
            assert_eq!(wait(&rx).text, "Copied /srv/x (via terminal, OSC 52)");
            assert_eq!(term, osc52("/srv/x").into_bytes());
            assert_eq!(fake.log(), "", "no native tool over SSH");
        }

        #[test]
        #[ignore]
        fn open_passes_path_as_one_argument() {
            let fake = FakeTools::install();
            let (tx, rx) = mpsc::channel();
            open(Path::new("/data/My Files/a.txt"), &tx);
            let n = wait(&rx);
            assert!(!n.error, "{n:?}");
            assert_eq!(n.text, "Opened /data/My Files/a.txt");
            let opener = if cfg!(target_os = "macos") {
                "open"
            } else {
                "xdg-open"
            };
            assert_eq!(fake.log(), format!("{opener} /data/My Files/a.txt|\n"));
        }
    }
}

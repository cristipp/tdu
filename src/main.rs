//! `tdu [OPTIONS] [PATH]`; see `--help`.

mod platform;

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    KeyboardEnhancementFlags, MouseButton, MouseEventKind, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::supports_keyboard_enhancement;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Gauge, Paragraph};
use ratatui::{DefaultTerminal, Frame};

use platform::{ClickTracker, Notice, Os};
use tdu::{
    ColorBy, ColorMode, Direction, DiskTree, Nesting, NodeId, ProgressSnapshot, ScanOptions,
    ScanProgress, SizeMode, TreeMap, TreeMapState, format_size, scan_with_progress,
};

/// How long a status notice stays visible.
const NOTICE_TTL: Duration = Duration::from_secs(4);
/// Progress screen refresh interval.
const PROGRESS_TICK: Duration = Duration::from_millis(80);
/// Scans that finish faster than this never show the progress screen.
const PROGRESS_DELAY: Duration = Duration::from_millis(150);

const USAGE: &str = "\
usage: tdu [OPTIONS] [PATH]

  --allocated                size on disk (like du) instead of file length
  -x, --one-file-system      don't cross mount points
  --no-skip-virtual          also scan /proc, /sys, devfs, autofs, ...
  --nesting flat|header      flat: files only, exact proportions (default)
                             header: nested directory boxes with title bars
  --color top|ext|depth      colour by top-level dir, extension, or depth
  --colors truecolor|256|16  terminal colour depth (default: auto-detect)
  -h, --help";

struct App {
    tree: DiskTree,
    state: TreeMapState,
    color_by: ColorBy,
    color_mode: ColorMode,
    nesting: Nesting,
    clicks: ClickTracker<NodeId>,
    /// Results of background actions (copy, open) arrive here.
    notices: (mpsc::Sender<Notice>, mpsc::Receiver<Notice>),
    notice: Option<(Notice, Instant)>,
    /// Help-line label for the copy shortcut ("⌘C/y", "^⇧C/y" or "y").
    copy_label: &'static str,
    /// Set while mouse capture is off so the terminal can select text; holds
    /// the reason shown in the status bar. Any key turns capture back on.
    released: Option<String>,
    /// Where the selection's path is drawn (clicking it releases the mouse).
    path_area: Rect,
}

impl App {
    fn selected_path(&self) -> Option<PathBuf> {
        self.state.selected().map(|n| self.tree.path_of(n))
    }

    /// Cmd-C / Ctrl-Shift-C / y: copy the selection's absolute path.
    fn copy_selected(&mut self) -> io::Result<()> {
        if let Some(path) = self.selected_path()
            && let Some(why) =
                platform::copy(&path.to_string_lossy(), &mut io::stdout(), &self.notices.0)
        {
            // The clipboard may not have got it (e.g. Terminal.app over ssh,
            // which has no way to receive one): let the user select the path.
            self.release_mouse(why)?;
        }
        Ok(())
    }

    /// Turn mouse capture off so the terminal's own drag / double-click /
    /// triple-click selection works (terminals can't mix the two).
    fn release_mouse(&mut self, why: String) -> io::Result<()> {
        if self.released.is_none() {
            execute!(io::stdout(), DisableMouseCapture)?;
        }
        self.released = Some(why);
        Ok(())
    }

    fn capture_mouse(&mut self) -> io::Result<()> {
        if self.released.take().is_some() {
            execute!(io::stdout(), EnableMouseCapture)?;
        }
        Ok(())
    }

    /// Double-click / `o`: open the selection with the default application.
    fn open_selected(&mut self) {
        if let Some(path) = self.selected_path() {
            platform::open(&path, &self.notices.0);
        }
    }
}

fn main() -> io::Result<()> {
    let mut opts = ScanOptions::default();
    let mut color_mode = ColorMode::detect();
    let mut color_by = ColorBy::TopLevel;
    let mut nesting = Nesting::Flat;
    let mut path = PathBuf::from(".");
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--allocated" => opts.size_mode = SizeMode::Allocated,
            "--one-file-system" | "-x" => opts.one_file_system = true,
            "--color" => {
                color_by = match args.next().as_deref() {
                    Some("ext") => ColorBy::Extension,
                    Some("depth") => ColorBy::Depth,
                    _ => ColorBy::TopLevel,
                }
            }
            "--nesting" => {
                nesting = match args.next().as_deref() {
                    Some("header") => Nesting::Header,
                    Some("flat") => Nesting::Flat,
                    v => usage_error(&format!("bad --nesting value {v:?}")),
                }
            }
            "--no-skip-virtual" => opts.skip_virtual_fs = false,
            "--colors" => {
                let v = args.next().unwrap_or_default();
                match ColorMode::parse(&v) {
                    Some(m) => color_mode = m,
                    None => usage_error(&format!("bad --colors value {v:?}")),
                }
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            p if p.starts_with('-') && p.len() > 1 => usage_error(&format!("unknown option {p}")),
            p => path = PathBuf::from(p),
        }
    }

    // Fail fast, before taking over the terminal, on a path that doesn't exist.
    if let Err(e) = std::fs::metadata(&path) {
        eprintln!("tdu: {}: {e}", path.display());
        std::process::exit(1);
    }

    // `ratatui::run` restores raw mode and the alternate screen on panic, but
    // not mouse capture. Chain a hook so a crash doesn't leave the shell
    // swallowing mouse events. (Keyboard-protocol flags live on the alternate
    // screen's own stack, so leaving it resets them.)
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(io::stdout(), DisableMouseCapture);
        prev_hook(info);
    }));

    // `ratatui::run` enters raw mode + the alternate screen, installs a panic
    // hook, and restores the terminal afterwards (even on panic).
    ratatui::run(|terminal| {
        // Cmd-C / Ctrl-Shift-C are only reported with the kitty keyboard
        // protocol. Enable it only where the terminal says it's supported.
        let enhanced = supports_keyboard_enhancement().unwrap_or(false);
        execute!(io::stdout(), EnableMouseCapture)?;
        if enhanced {
            execute!(
                io::stdout(),
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
            )?;
        }
        let res = (|| {
            let Some((tree, elapsed)) = scan_screen(terminal, &path, &opts)? else {
                return Ok(()); // cancelled
            };
            let summary = Notice::ok(format!(
                "Scanned {} entries in {:.1?}{}",
                thousands(tree.len() as u64 - 1),
                elapsed,
                match tree.errors.len() {
                    0 => String::new(),
                    n => format!(" ({n} unreadable)"),
                }
            ));
            let state = TreeMapState::new(tree.root());
            let mut app = App {
                tree,
                state,
                color_by,
                color_mode,
                nesting,
                clicks: ClickTracker::default(),
                notices: mpsc::channel(),
                notice: Some((summary, Instant::now())),
                copy_label: platform::copy_key_label(Os::current(), enhanced),
                released: None,
                path_area: Rect::default(),
            };
            run(terminal, &mut app)
        })();
        if enhanced {
            execute!(io::stdout(), PopKeyboardEnhancementFlags)?;
        }
        execute!(io::stdout(), DisableMouseCapture)?;
        res
    })
}

/// Scan on a background thread while showing a progress screen. Returns
/// `Ok(None)` if the user cancels with `q`, `Esc` or `Ctrl-C`.
fn scan_screen(
    terminal: &mut DefaultTerminal,
    path: &Path,
    opts: &ScanOptions,
) -> io::Result<Option<(DiskTree, Duration)>> {
    let progress = Arc::new(ScanProgress::new());
    let start = Instant::now();
    let worker = {
        let (progress, path, opts) = (progress.clone(), path.to_path_buf(), opts.clone());
        std::thread::spawn(move || scan_with_progress(&path, &opts, &progress))
    };
    loop {
        if worker.is_finished() {
            return match worker.join() {
                Ok(Ok(tree)) => Ok(Some((tree, start.elapsed()))),
                Ok(Err(e)) if e.kind() == io::ErrorKind::Interrupted => Ok(None),
                Ok(Err(e)) => Err(e),
                Err(_) => Err(io::Error::other("scanner thread panicked")),
            };
        }
        let elapsed = start.elapsed();
        // Don't flash a progress screen for scans that finish instantly.
        let tick = if elapsed < PROGRESS_DELAY {
            Duration::from_millis(10)
        } else {
            let snap = progress.snapshot();
            let cancelling = progress.is_cancelled();
            terminal.draw(|f| draw_progress(f, path, &snap, elapsed, cancelling))?;
            PROGRESS_TICK
        };
        if event::poll(tick)?
            && let Event::Key(k) = event::read()?
            && k.kind == KeyEventKind::Press
            && (matches!(k.code, KeyCode::Char('q') | KeyCode::Esc)
                || (k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL)))
        {
            progress.cancel();
        }
    }
}

/// The scan progress screen: a centred box with an estimated-progress gauge,
/// live counters, throughput and the directory being read.
fn draw_progress(
    f: &mut Frame,
    path: &Path,
    p: &ProgressSnapshot,
    elapsed: Duration,
    cancelling: bool,
) {
    let area = f.area();
    let w = area.width.min(76);
    let h = area.height.min(8);
    let r = Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Line::from(vec![
            " tdu ".black().on_cyan(),
            Span::raw(" scanning "),
            Span::styled(
                path.display().to_string(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
        ]));
    let inner = block.inner(r);
    f.render_widget(Clear, r);
    f.render_widget(block, r);
    if inner.height == 0 || inner.width < 4 {
        return;
    }
    let [
        gauge_area,
        _,
        stats_area,
        rate_area,
        current_area,
        hint_area,
    ] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);

    // The fraction is an estimate (sizes aren't known until the scan ends),
    // hence the "≈".
    let gauge = Gauge::default()
        .ratio(p.fraction.clamp(0.0, 1.0))
        .label(format!("≈ {:.0}%", p.fraction * 100.0))
        .use_unicode(true)
        .gauge_style(Style::default().fg(Color::Cyan).bg(Color::DarkGray));
    f.render_widget(gauge, gauge_area);

    let mut stats = format!(
        "{} entries · {} · {} dirs",
        thousands(p.entries),
        format_size(p.bytes),
        thousands(p.dirs)
    );
    if p.errors > 0 {
        stats.push_str(&format!(" · {} unreadable", thousands(p.errors)));
    }
    f.render_widget(Paragraph::new(stats), stats_area);

    let secs = elapsed.as_secs_f64();
    let rate = if secs > 0.0 {
        p.entries as f64 / secs
    } else {
        0.0
    };
    let dim = Style::default().fg(Color::DarkGray);
    f.render_widget(
        Paragraph::new(Span::styled(
            format!("{secs:.1} s · {} entries/s", thousands(rate as u64)),
            dim,
        )),
        rate_area,
    );
    f.render_widget(
        Paragraph::new(Span::styled(
            truncate_left(&p.current.display().to_string(), inner.width as usize),
            dim,
        )),
        current_area,
    );
    let hint = if cancelling {
        Span::styled("cancelling…", Style::default().fg(Color::Yellow))
    } else {
        Span::styled("q / Esc / Ctrl-C to cancel", dim)
    };
    f.render_widget(Paragraph::new(hint), hint_area);
}

/// `1234567` -> `"1,234,567"`.
fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Keep the end of `s` (the most specific part of a path) within `w` columns.
fn truncate_left(s: &str, w: usize) -> String {
    let n = s.chars().count();
    if n <= w {
        s.to_string()
    } else if w == 0 {
        String::new()
    } else {
        let tail: String = s.chars().skip(n - (w - 1)).collect();
        format!("…{tail}")
    }
}

/// Quote `s` for a POSIX shell the way Terminal.app does on drag-and-drop:
/// backslash before spaces and other special characters (`/My\ Files/a\(1\)`).
/// Paths with control characters (newlines, tabs, …) are single-quoted
/// instead, since a backslash-newline would be a line continuation.
fn shell_quote(s: &str) -> String {
    let safe = |c: char| c.is_alphanumeric() || "/._-+,:@%=~^".contains(c);
    if s.chars().any(char::is_control) {
        return format!("'{}'", s.replace('\'', r"'\''"));
    }
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        if !safe(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn usage_error(msg: &str) -> ! {
    eprintln!("tdu: {msg}\n\n{USAGE}");
    std::process::exit(2);
}

fn run(terminal: &mut DefaultTerminal, app: &mut App) -> io::Result<()> {
    loop {
        while let Ok(n) = app.notices.1.try_recv() {
            app.notice = Some((n, Instant::now()));
        }
        if app
            .notice
            .as_ref()
            .is_some_and(|(_, t)| t.elapsed() > NOTICE_TTL)
        {
            app.notice = None;
        }
        terminal.draw(|f| ui(f, app))?;
        // Wake up periodically so notices from background actions show up
        // (and expire) without waiting for input.
        if !event::poll(Duration::from_millis(200))? {
            continue;
        }
        let ev = event::read()?;
        // Any key ends text-selection mode; `m` only toggles it.
        if let Event::Key(k) = &ev
            && k.kind == KeyEventKind::Press
        {
            let toggle = k.code == KeyCode::Char('m') && k.modifiers.is_empty();
            if app.released.is_some() {
                app.capture_mouse()?;
                if toggle {
                    continue;
                }
            } else if toggle {
                app.release_mouse("Selecting text".into())?;
                continue;
            }
        }
        match ev {
            Event::Key(k) if k.kind == KeyEventKind::Press && is_copy(&k) => app.copy_selected()?,
            Event::Key(k) if k.kind == KeyEventKind::Press => match k.code {
                KeyCode::Char('q') => return Ok(()),
                // Raw mode delivers Ctrl-C as a key, not SIGINT.
                KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(());
                }
                KeyCode::Char('o') => app.open_selected(),
                KeyCode::Left | KeyCode::Char('h') => {
                    app.state.move_selection(Direction::Left);
                }
                KeyCode::Right | KeyCode::Char('l') => {
                    app.state.move_selection(Direction::Right);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    app.state.move_selection(Direction::Up);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    app.state.move_selection(Direction::Down);
                }
                // Enter a directory (select its largest child) / go back up.
                // Where there's no directory tile to enter (flat mode, or a
                // file), zoom into the directory instead.
                KeyCode::Enter | KeyCode::Char(']') => {
                    if !app.state.select_child() {
                        app.state.zoom_in(&app.tree);
                    }
                }
                KeyCode::Backspace | KeyCode::Char('[') => {
                    if !app.state.select_parent() {
                        app.state.zoom_out(&app.tree);
                    }
                }
                // Make the selected directory fill the view / undo.
                KeyCode::Char('z') | KeyCode::Char('+') => {
                    app.state.zoom_in(&app.tree);
                }
                KeyCode::Char('Z') | KeyCode::Char('-') | KeyCode::Esc => {
                    app.state.zoom_out(&app.tree);
                }
                KeyCode::Char('n') => {
                    app.nesting = match app.nesting {
                        Nesting::Flat => Nesting::Header,
                        Nesting::Header => Nesting::Flat,
                    }
                }
                KeyCode::Char('c') if k.modifiers.is_empty() => {
                    app.color_by = match app.color_by {
                        ColorBy::TopLevel => ColorBy::Extension,
                        ColorBy::Extension => ColorBy::Depth,
                        ColorBy::Depth => ColorBy::TopLevel,
                    }
                }
                _ => {}
            },
            Event::Mouse(m) => match m.kind {
                MouseEventKind::Down(MouseButton::Left)
                    if app.path_area.contains((m.column, m.row).into()) =>
                {
                    app.release_mouse("Selecting text".into())?;
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(hit) = app.state.select_at(m.column, m.row)
                        && app.clicks.press(Instant::now(), hit)
                    {
                        app.open_selected();
                    }
                }
                MouseEventKind::Down(MouseButton::Right) => {
                    app.state.zoom_out(&app.tree);
                }
                MouseEventKind::ScrollUp => {
                    // Zoom into the directory under the pointer.
                    if let Some(hit) = app.state.select_at(m.column, m.row) {
                        let dir = app.tree.child_toward(app.state.root(), hit);
                        app.state.select(dir.or(Some(hit)));
                        app.state.zoom_in(&app.tree);
                    }
                }
                MouseEventKind::ScrollDown => {
                    app.state.zoom_out(&app.tree);
                }
                _ => {}
            },
            _ => {}
        }
    }
}

fn is_copy(k: &event::KeyEvent) -> bool {
    platform::is_copy_key(Os::current(), k.code, k.modifiers)
}

fn ui(f: &mut Frame, app: &mut App) {
    let [header_area, map_area, status_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(2),
    ])
    .areas(f.area());
    let tree = &app.tree;

    // Header: view root.
    let root = tree.node(app.state.root());
    let header = Line::from(vec![
        " tdu ".black().on_cyan(),
        Span::raw(" "),
        Span::styled(
            tree.path_of(app.state.root()).display().to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(
            "  {}  {} files",
            format_size(root.size),
            root.file_count
        )),
        match tree.errors.len() {
            0 => Span::raw(""),
            n => Span::styled(
                format!("  {n} unreadable"),
                Style::default().fg(Color::Yellow),
            ),
        },
    ]);
    f.render_widget(Paragraph::new(header), header_area);

    // The map.
    let map = TreeMap::new(tree)
        .color_by(app.color_by)
        .color_mode(app.color_mode)
        .nesting(app.nesting);
    f.render_stateful_widget(map, map_area, &mut app.state);

    // Status: selection + hidden stats + keys.
    let dim = Style::default().fg(Color::DarkGray);
    let mut sel_line = vec![Span::raw(" ")];
    if let Some(sel) = app.state.selected() {
        let n = tree.node(sel);
        let pct = |of: u64| {
            if of == 0 {
                0.0
            } else {
                n.size as f64 * 100.0 / of as f64
            }
        };
        let mut path = tree.path_of(sel).display().to_string();
        let mut files = String::new();
        if n.is_dir() {
            path.push('/');
            files = format!("  {} files", n.file_count);
        }
        // Shown ready to paste into a shell (for mouse selection).
        let path = shell_quote(&path);
        sel_line.push(Span::styled(
            path,
            Style::default().add_modifier(Modifier::BOLD),
        ));
        sel_line.push(Span::raw(format!(
            "  {}{files}  {:.2}% of view  {:.2}% of total",
            format_size(n.size),
            pct(root.size),
            pct(tree.node(tree.root()).size),
        )));
    }
    let hidden = app.state.layout().map(|l| l.hidden).unwrap_or_default();
    // A fresh notice (copied / opened / error) replaces the hidden-files count.
    // In text-selection mode the line stays fixed, so redraws don't disturb
    // the terminal's selection.
    let info = match (&app.released, &app.notice) {
        (Some(why), _) => Span::styled(
            format!(
                " {why}: drag / double-click / triple-click to select, then ⌘C · any key resumes"
            ),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        (None, Some((n, _))) => Span::styled(
            format!(" {}", n.text),
            Style::default()
                .fg(if n.error { Color::Red } else { Color::Green })
                .add_modifier(Modifier::BOLD),
        ),
        (None, None) => Span::styled(
            format!(
                " {} files ({}) not shown",
                hidden.files,
                format_size(hidden.bytes)
            ),
            dim,
        ),
    };
    let mut keys = vec![info];
    if app.released.is_none() {
        keys.push(Span::styled(
            format!(
                "  ·  ←↓↑→ move  ⏎ enter  ⌫ up  z/Z zoom  {} copy path  m/click path select text  o/dbl-click open  n nesting  c colours  q quit",
                app.copy_label
            ),
            dim,
        ));
    }
    let keys = Line::from(keys);
    app.path_area = Rect {
        height: 1,
        ..status_area
    };
    f.render_widget(
        Paragraph::new(vec![Line::from(sel_line), keys]),
        status_area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn formats_thousands() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(thousands(1234567), "1,234,567");
    }

    #[test]
    fn truncates_paths_from_the_left() {
        assert_eq!(truncate_left("/a/b/c", 10), "/a/b/c");
        assert_eq!(truncate_left("/very/long/path/file", 8), "…th/file");
        assert_eq!(truncate_left("/x", 0), "");
    }

    #[test]
    fn shell_quotes_paths() {
        assert_eq!(shell_quote("/usr/local/bin/"), "/usr/local/bin/");
        assert_eq!(
            shell_quote("/My Files/a (1).txt"),
            r"/My\ Files/a\ \(1\).txt"
        );
        assert_eq!(shell_quote("/it's $HOME&*"), r"/it\'s\ \$HOME\&\*");
        assert_eq!(shell_quote("/café/naïve"), "/café/naïve");
        assert_eq!(shell_quote("/a\nb's"), "'/a\nb'\\''s'");
    }

    #[test]
    fn status_bar_select_text_mode() {
        let dir = std::env::temp_dir().join(format!("tdu ui {}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("my file.txt"), "x").unwrap();
        let tree = tdu::scan(&dir, &ScanOptions::default()).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        let state = TreeMapState::new(tree.root());
        let mut app = App {
            tree,
            state,
            color_by: ColorBy::TopLevel,
            color_mode: ColorMode::TrueColor,
            nesting: Nesting::Flat,
            clicks: ClickTracker::default(),
            notices: mpsc::channel(),
            notice: None,
            copy_label: "y",
            released: None,
            path_area: Rect::default(),
        };
        let mut term = Terminal::new(TestBackend::new(200, 20)).unwrap();
        let text = |term: &Terminal<TestBackend>| -> String {
            term.backend()
                .buffer()
                .content()
                .iter()
                .map(|c| c.symbol())
                .collect()
        };
        term.draw(|f| ui(f, &mut app)).unwrap();
        // The path line is the first status row: clicking it selects text.
        assert_eq!(app.path_area, Rect::new(0, 18, 200, 1));
        let t = text(&term);
        assert!(t.contains("m/click path select text"));
        assert!(
            t.contains(r"tdu\ ui\ ") && t.contains(r"/my\ file.txt  "),
            "{t}"
        );
        app.released = Some("Selecting text".into());
        term.draw(|f| ui(f, &mut app)).unwrap();
        let t = text(&term);
        assert!(t.contains("Selecting text: drag"), "{t}");
        // Still shell-quoted while selecting.
        assert!(
            t.contains(r"tdu\ ui\ ") && t.contains(r"/my\ file.txt  "),
            "{t}"
        );
        assert!(!t.contains("q quit"));
    }

    #[test]
    fn progress_screen_renders() {
        let snap = ProgressSnapshot {
            entries: 123_456,
            bytes: 5 << 30,
            dirs: 7_890,
            errors: 3,
            fraction: 0.42,
            current: PathBuf::from("/home/me/projects/deeply/nested/dir"),
        };
        let mut term = Terminal::new(TestBackend::new(80, 12)).unwrap();
        term.draw(|f| {
            draw_progress(
                f,
                Path::new("/home/me"),
                &snap,
                Duration::from_secs(2),
                false,
            )
        })
        .unwrap();
        let text: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        for want in [
            "scanning /home/me",
            "≈ 42%",
            "123,456 entries · 5.0 GiB · 7,890 dirs · 3 unreadable",
            "61,728 entries/s",
            "/home/me/projects/deeply/nested/dir",
            "q / Esc / Ctrl-C to cancel",
        ] {
            assert!(text.contains(want), "missing {want:?}");
        }
        // Tiny terminals don't panic.
        let mut tiny = Terminal::new(TestBackend::new(3, 2)).unwrap();
        tiny.draw(|f| draw_progress(f, Path::new("/"), &snap, Duration::ZERO, true))
            .unwrap();
    }
}

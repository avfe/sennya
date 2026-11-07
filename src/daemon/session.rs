//! One terminal session: a PTY, its shell and the terminal emulator state.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io;
#[cfg(unix)]
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Boundary, Column, Direction, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::search::RegexSearch;
use alacritty_terminal::term::{self, viewport_to_point, Osc52, Term, TermMode};
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::{Color as VColor, CursorShape as VShape, CursorStyle, NamedColor, Processor};

use crate::config::{Config, CursorStyleCfg};
use crate::model::SessionId;
use crate::protocol::{
    flags, Color, Cursor, CursorShape, Frame, Line as FLine, Run, ScrollCmd, SearchHit, SearchQuery,
    SelectCmd, SelectKind, SessionInfo, NAMED_BG,
};

use super::procinfo;

#[derive(Clone, Copy)]
pub struct TermSize {
    pub cols: usize,
    pub rows: usize,
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// Forwards emulator events to the daemon's dispatcher thread.
#[derive(Clone)]
pub struct Listener {
    pub id: SessionId,
    pub tx: Sender<(SessionId, Event)>,
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let _ = self.tx.send((self.id, event));
    }
}

pub struct Meta {
    pub osc_title: String,
    pub cwd: String,
    pub process: String,
    pub cols: u16,
    pub rows: u16,
    pub cell_w: u16,
    pub cell_h: u16,
    pub restored: bool,
    pub fg_pid: i32,
    /// Claude Code conversation running here (or that ran last).
    pub claude: String,
    pub claude_title: String,
    pub claude_running: bool,
    pub claude_since: Option<std::time::SystemTime>,
    /// One of the `CLAUDE_*` states, read from the screen.
    pub claude_state: u8,
    /// Options of the running `claude` command, reused when its chat is resumed.
    pub claude_flags: Vec<String>,
    /// Ready at its prompt since then (for hibernation).
    pub claude_idle_since: Option<std::time::Instant>,
    /// Claude was closed to save memory; its chat reopens when the terminal is viewed.
    pub claude_hibernated: bool,
    /// `/exit` was typed to hibernate; the chat counts as asleep once Claude has really quit.
    pub claude_exit_requested: Option<std::time::Instant>,
}

pub const CLAUDE_UNKNOWN: u8 = 0;
/// Waiting for the next prompt.
pub const CLAUDE_READY: u8 = 1;
pub const CLAUDE_WORKING: u8 = 2;
/// Asking for permission or a choice.
pub const CLAUDE_NEEDS_YOU: u8 = 3;
/// Closed to save memory; resumes when viewed.
pub const CLAUDE_ASLEEP: u8 = 4;

/// What Claude Code is doing, judged from its screen.
pub fn detect_claude(lines: &[String]) -> u8 {
    let mut working = false;
    let mut ready = false;
    let mut prev = "";
    for line in lines {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let lower = t.to_lowercase();
        let numbered_choice = t
            .strip_prefix('❯')
            .map(str::trim_start)
            .and_then(|r| r.split_once(". "))
            .is_some_and(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
        if numbered_choice || lower.contains("do you want to") || lower.contains("would you like to") || lower.contains("do you trust") {
            return CLAUDE_NEEDS_YOU;
        }
        // In a narrow pane the hint is cut ("esc to int…"); the spinner line
        // ("✻ Sketching… (20s · ↓ 1.1k tokens)") says the same.
        let spinner = t.starts_with(['✻', '✽', '✶', '✳', '✢', '·', '*', '◐', '◓', '◑', '◒'])
            && t.contains('…')
            && t.contains('(');
        if lower.contains("esc to int") || spinner {
            working = true;
        }
        if t.starts_with('❯') && prev.starts_with('─') {
            ready = true;
        }
        prev = t;
    }
    if working {
        CLAUDE_WORKING
    } else if ready {
        CLAUDE_READY
    } else {
        CLAUDE_UNKNOWN
    }
}

pub struct Session {
    pub id: SessionId,
    pub term: Arc<FairMutex<Term<Listener>>>,
    pub tx: EventLoopSender,
    pub pid: u32,
    /// The PTY master, for asking which process group is in the foreground.
    #[cfg(unix)]
    master: OwnedFd,
    pub meta: Mutex<Meta>,
    /// New output since the last scrollback snapshot.
    pub unsaved: AtomicBool,
    /// An `Activity` notification was already sent for unseen output.
    pub activity_sent: AtomicBool,
    /// When the program last printed anything (milliseconds since the daemon started).
    pub last_output: std::sync::atomic::AtomicU64,
    pub spawned: std::time::Instant,
    /// Some client is showing this terminal.
    pub watched: AtomicBool,
}

pub struct SpawnOpts<'a> {
    pub id: SessionId,
    pub cwd: Option<String>,
    pub cols: u16,
    pub rows: u16,
    /// Saved scrollback to replay before the shell starts.
    pub restore: Option<Vec<u8>>,
    pub config: &'a Config,
}

pub fn term_config(cfg: &Config) -> term::Config {
    term::Config {
        scrolling_history: cfg.terminal.scrollback,
        default_cursor_style: CursorStyle {
            shape: match cfg.terminal.cursor_style {
                CursorStyleCfg::Block => VShape::Block,
                CursorStyleCfg::Beam => VShape::Beam,
                CursorStyleCfg::Underline => VShape::Underline,
            },
            blinking: cfg.terminal.cursor_blink,
        },
        osc52: if cfg.security.osc52_copy { Osc52::OnlyCopy } else { Osc52::Disabled },
        ..Default::default()
    }
}

impl Session {
    pub fn spawn(opts: SpawnOpts<'_>, events: Sender<(SessionId, Event)>) -> io::Result<Arc<Session>> {
        let cfg = opts.config;
        let cols = opts.cols.clamp(2, 1000);
        let rows = opts.rows.clamp(1, 500);
        let listener = Listener { id: opts.id, tx: events };
        let size = TermSize { cols: cols as usize, rows: rows as usize };
        let mut term = Term::new(term_config(cfg), &size, listener.clone());
        let restored = opts.restore.is_some();
        if let Some(bytes) = opts.restore {
            let mut parser: Processor = Processor::new();
            parser.advance(&mut term, &bytes);
        }
        let term = Arc::new(FairMutex::new(term));

        let mut env: HashMap<String, String> = HashMap::new();
        env.insert("TERM".into(), "xterm-256color".into());
        env.insert("COLORTERM".into(), "truecolor".into());
        env.insert("TERM_PROGRAM".into(), "sennya".into());
        env.insert("TERM_PROGRAM_VERSION".into(), env!("CARGO_PKG_VERSION").into());
        env.insert("SENNYA_SESSION".into(), opts.id.to_string());
        if std::env::var_os("LANG").is_none() {
            // Apps launched from Finder get no locale; without it zsh mangles non-ASCII input.
            env.insert("LANG".into(), "en_US.UTF-8".into());
        }
        for (k, v) in &cfg.env {
            env.insert(k.clone(), v.clone());
        }

        let shell = (!cfg.shell.program.is_empty())
            .then(|| tty::Shell::new(cfg.shell.program.clone(), cfg.shell.args.clone()));
        let cwd = opts
            .cwd
            .filter(|p| Path::new(p).is_dir())
            .map(PathBuf::from)
            .or_else(dirs::home_dir);
        let cwd_string = cwd.as_ref().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        let pty_opts = tty::Options {
            shell,
            working_directory: cwd,
            drain_on_exit: false,
            env,
            #[cfg(windows)]
            escape_args: true,
        };
        let window = WindowSize { num_lines: rows, num_cols: cols, cell_width: 8, cell_height: 16 };
        let pty = tty::new(&pty_opts, window, opts.id)?;
        #[cfg(unix)]
        let pid = pty.child().id();
        #[cfg(unix)]
        let master = pty.file().as_fd().try_clone_to_owned()?;
        #[cfg(windows)]
        let pid = pty.child_watcher().pid().map(|p| p.get()).unwrap_or(0);
        let event_loop = EventLoop::new(term.clone(), listener, pty, false, false)?;
        let tx = event_loop.channel();
        event_loop.spawn();

        Ok(Arc::new(Session {
            id: opts.id,
            term,
            tx,
            pid,
            #[cfg(unix)]
            master,
            meta: Mutex::new(Meta {
                osc_title: String::new(),
                cwd: cwd_string,
                process: String::new(),
                cols,
                rows,
                cell_w: 8,
                cell_h: 16,
                restored,
                fg_pid: 0,
                claude: String::new(),
                claude_title: String::new(),
                claude_running: false,
                claude_since: None,
                claude_state: CLAUDE_UNKNOWN,
                claude_flags: Vec::new(),
                claude_idle_since: None,
                claude_hibernated: false,
                claude_exit_requested: None,
            }),
            unsaved: AtomicBool::new(restored),
            activity_sent: AtomicBool::new(false),
            last_output: std::sync::atomic::AtomicU64::new(0),
            spawned: std::time::Instant::now(),
            watched: AtomicBool::new(false),
        }))
    }

    pub fn info(&self) -> SessionInfo {
        let meta = self.meta.lock().unwrap();
        SessionInfo {
            id: self.id,
            osc_title: meta.osc_title.clone(),
            cwd: meta.cwd.clone(),
            process: meta.process.clone(),
            shell_pid: self.pid,
            restored: meta.restored,
        }
    }

    /// Refresh cwd / foreground process. Returns true when something changed.
    ///
    /// The PTY's child is often a wrapper (`/usr/bin/login` on macOS) whose folder never
    /// changes, so look at the terminal's foreground process instead: the shell itself
    /// while it waits for input, otherwise the program it runs.
    pub fn poll_process(&self) -> bool {
        #[cfg(unix)]
        let fg = procinfo::foreground_pid(self.master.as_raw_fd());
        #[cfg(windows)]
        let fg = procinfo::foreground_pid(self.pid as i32);
        let cwd = fg.and_then(procinfo::cwd).or_else(|| procinfo::cwd(self.pid as i32));
        let process = match fg {
            Some(fg) if fg != self.pid as i32 => {
                let name = procinfo::display_name(fg).unwrap_or_default();
                if procinfo::is_shell(&name) { String::new() } else { name }
            }
            _ => String::new(),
        };
        let mut meta = self.meta.lock().unwrap();
        meta.fg_pid = fg.unwrap_or(0);
        let mut changed = false;
        if let Some(cwd) = cwd {
            if cwd != meta.cwd {
                meta.cwd = cwd;
                changed = true;
            }
        }
        if process != meta.process {
            meta.process = process;
            changed = true;
        }
        changed
    }

    /// Text of the line the cursor is on, without trailing blanks.
    pub fn cursor_line(&self) -> String {
        let term = self.term.lock();
        let line = term.grid().cursor.point.line;
        let row = &term.grid()[line];
        let text: String = (0..term.columns()).map(|c| row[Column(c)].c).collect();
        text.trim_end().to_string()
    }

    /// Lines of the screen (not of the scrolled view).
    pub fn screen_lines(&self) -> Vec<String> {
        let term = self.term.lock();
        let grid = term.grid();
        (0..term.screen_lines() as i32)
            .map(|l| {
                let row = &grid[Line(l)];
                (0..term.columns()).map(|c| row[Column(c)].c).collect::<String>().trim_end().to_string()
            })
            .collect()
    }

    /// Whether Claude Code's input box holds no draft (its placeholder is drawn dim).
    pub fn claude_input_empty(&self) -> bool {
        let term = self.term.lock();
        let grid = term.grid();
        let cols = term.columns();
        let mut prev_rule = false;
        for l in 0..term.screen_lines() as i32 {
            let row = &grid[Line(l)];
            let first = (0..cols).map(|c| &row[Column(c)]).find(|cell| cell.c != ' ');
            if prev_rule && first.is_some_and(|cell| cell.c == '❯') {
                let start = (0..cols).position(|c| row[Column(c)].c == '❯').unwrap_or(0) + 1;
                return (start..cols)
                    .map(|c| &row[Column(c)])
                    .filter(|cell| !matches!(cell.c, ' ' | '\0'))
                    .all(|cell| cell.flags.intersects(Flags::DIM | Flags::ITALIC));
            }
            prev_rule = first.is_some_and(|cell| cell.c == '─');
        }
        false
    }

    pub fn claude_state(&self) -> u8 {
        detect_claude(&self.screen_lines())
    }

    /// Whether anything has been printed on the screen yet (e.g. the shell prompt).
    pub fn has_text(&self) -> bool {
        let term = self.term.lock();
        let grid = term.grid();
        (0..term.screen_lines() as i32).any(|l| {
            let row = &grid[Line(l)];
            (0..term.columns()).any(|c| !matches!(row[Column(c)].c, ' ' | '\0'))
        })
    }

    /// The last `lines` lines (history included) as plain text.
    pub fn text(&self, lines: usize) -> String {
        let term = self.term.lock();
        let top = term.topmost_line().0;
        let last = term.last_column();
        // Count from the last line with content (or the cursor), not from the empty bottom.
        let grid = term.grid();
        let cursor = grid.cursor.point.line.0;
        let mut bottom = term.bottommost_line().0;
        while bottom > cursor && (0..term.columns()).all(|c| matches!(grid[Line(bottom)][Column(c)].c, ' ' | '\0')) {
            bottom -= 1;
        }
        let start = top.max(bottom - lines.max(1) as i32 + 1);
        let mut out: Vec<String> = (start..=bottom)
            .map(|l| term.bounds_to_string(Point::new(Line(l), Column(0)), Point::new(Line(l), last)).trim_end().to_string())
            .collect();
        while out.last().is_some_and(|l| l.is_empty()) {
            out.pop();
        }
        out.join("\n")
    }

    pub fn write(&self, bytes: Vec<u8>) {
        let _ = self.tx.send(Msg::Input(bytes.into()));
    }

    pub fn input(&self, bytes: Vec<u8>) {
        {
            let mut term = self.term.lock();
            if term.grid().display_offset() != 0 {
                term.scroll_display(Scroll::Bottom);
            }
        }
        self.write(bytes);
    }

    pub fn resize(&self, cols: u16, rows: u16, cell_w: u16, cell_h: u16) -> bool {
        let cols = cols.clamp(2, 1000);
        let rows = rows.clamp(1, 500);
        // One lock for the recorded size, the grid and the PTY, so two windows resizing at
        // once cannot leave them disagreeing.
        let mut meta = self.meta.lock().unwrap();
        if meta.cols == cols && meta.rows == rows && meta.cell_w == cell_w && meta.cell_h == cell_h {
            return false;
        }
        meta.cols = cols;
        meta.rows = rows;
        meta.cell_w = cell_w;
        meta.cell_h = cell_h;
        self.term.lock().resize(TermSize { cols: cols as usize, rows: rows as usize });
        let _ = self.tx.send(Msg::Resize(WindowSize { num_lines: rows, num_cols: cols, cell_width: cell_w, cell_height: cell_h }));
        true
    }

    pub fn window_size(&self) -> WindowSize {
        let meta = self.meta.lock().unwrap();
        WindowSize { num_lines: meta.rows, num_cols: meta.cols, cell_width: meta.cell_w, cell_height: meta.cell_h }
    }

    pub fn shutdown(&self) {
        let _ = self.tx.send(Msg::Shutdown);
    }

    pub fn scroll(&self, cmd: ScrollCmd) {
        let scroll = match cmd {
            ScrollCmd::Lines(n) => Scroll::Delta(n),
            ScrollCmd::PageUp => Scroll::PageUp,
            ScrollCmd::PageDown => Scroll::PageDown,
            ScrollCmd::Top => Scroll::Top,
            ScrollCmd::Bottom => Scroll::Bottom,
        };
        self.term.lock().scroll_display(scroll);
    }

    pub fn select(&self, cmd: SelectCmd) {
        let mut term = self.term.lock();
        let offset = term.grid().display_offset();
        let to_point = |term: &Term<Listener>, row: u16, col: u16| {
            let row = (row as usize).min(term.screen_lines().saturating_sub(1));
            let col = (col as usize).min(term.columns().saturating_sub(1));
            viewport_to_point(offset, Point::new(row, Column(col)))
        };
        match cmd {
            SelectCmd::Start { row, col, right, kind } => {
                let point = to_point(&term, row, col);
                let side = if right { Side::Right } else { Side::Left };
                let ty = match kind {
                    SelectKind::Simple => SelectionType::Simple,
                    SelectKind::Block => SelectionType::Block,
                    SelectKind::Word => SelectionType::Semantic,
                    SelectKind::Line => SelectionType::Lines,
                };
                term.selection = Some(Selection::new(ty, point, side));
            }
            SelectCmd::Update { row, col, right } => {
                let point = to_point(&term, row, col);
                let side = if right { Side::Right } else { Side::Left };
                if let Some(sel) = term.selection.as_mut() {
                    sel.update(point, side);
                }
            }
            SelectCmd::All => {
                let top = term.topmost_line();
                let bottom = term.bottommost_line();
                let last = term.last_column();
                let mut sel = Selection::new(SelectionType::Simple, Point::new(top, Column(0)), Side::Left);
                sel.update(Point::new(bottom, last), Side::Right);
                term.selection = Some(sel);
            }
            SelectCmd::Clear => term.selection = None,
        }
    }

    pub fn selection_text(&self) -> String {
        self.term.lock().selection_to_string().unwrap_or_default()
    }

    pub fn clear_scrollback(&self) {
        let alt = {
            let mut term = self.term.lock();
            term.grid_mut().clear_history();
            term.selection = None;
            term.mode().contains(TermMode::ALT_SCREEN)
        };
        if !alt {
            // Ask the shell to redraw its prompt at the top.
            self.write(vec![0x0c]);
        }
    }

    pub fn find(&self, query: &SearchQuery, backwards: bool) -> bool {
        let Some(mut regex) = build_regex(query) else { return false };
        let mut term = self.term.lock();
        let range = term.selection.as_ref().and_then(|s| s.to_range(&*term));
        let (origin, direction) = if backwards {
            let origin = match range {
                Some(r) => r.start.sub(&*term, Boundary::None, 1),
                None => Point::new(term.bottommost_line(), term.last_column()),
            };
            (origin, Direction::Left)
        } else {
            let origin = match range {
                Some(r) => r.end.add(&*term, Boundary::None, 1),
                None => Point::new(term.topmost_line(), Column(0)),
            };
            (origin, Direction::Right)
        };
        let side = if backwards { Side::Right } else { Side::Left };
        match term.search_next(&mut regex, origin, direction, side, None) {
            Some(m) => {
                let mut sel = Selection::new(SelectionType::Simple, *m.start(), Side::Left);
                sel.update(*m.end(), Side::Right);
                term.selection = Some(sel);
                term.scroll_to_point(*m.start());
                true
            }
            None => false,
        }
    }

    /// Every match in this terminal's history, newest first.
    pub fn search(&self, query: &SearchQuery, limit: usize) -> Vec<SearchHit> {
        let Some(mut regex) = build_regex(query) else { return Vec::new() };
        let term = self.term.lock();
        let start = Point::new(term.bottommost_line(), term.last_column());
        let end = Point::new(term.topmost_line(), Column(0));
        let iter = term::search::RegexIter::new(start, end, Direction::Left, &*term, &mut regex);
        let last_col = term.last_column();
        let mut hits = Vec::new();
        let mut seen_lines = std::collections::HashSet::new();
        for m in iter {
            let (s, e) = (*m.start(), *m.end());
            // One hit per line keeps the result list readable.
            if !seen_lines.insert(s.line.0) {
                continue;
            }
            let text = term.bounds_to_string(Point::new(s.line, Column(0)), Point::new(s.line, last_col));
            hits.push(SearchHit {
                session: self.id,
                line: s.line.0,
                start_col: s.column.0 as u16,
                end_line: e.line.0,
                end_col: e.column.0 as u16,
                snippet: text.trim_end().chars().take(300).collect(),
            });
            if hits.len() >= limit {
                break;
            }
        }
        hits
    }

    pub fn reveal(&self, hit: &SearchHit) {
        let mut term = self.term.lock();
        let top = term.topmost_line().0;
        let bottom = term.bottommost_line().0;
        if hit.line < top || hit.end_line > bottom {
            return;
        }
        let last = term.last_column().0;
        let start = Point::new(Line(hit.line), Column((hit.start_col as usize).min(last)));
        let end = Point::new(Line(hit.end_line), Column((hit.end_col as usize).min(last)));
        let mut sel = Selection::new(SelectionType::Simple, start, Side::Left);
        sel.update(end, Side::Right);
        term.selection = Some(sel);
        term.scroll_to_point(start);
    }

    /// Snapshot of the visible screen for the GUI.
    pub fn frame(&self) -> Frame {
        let term = self.term.lock();
        let content = term.renderable_content();
        let cols = term.columns();
        let rows = term.screen_lines();
        let offset = content.display_offset;
        let selection = content.selection;

        let cursor_point = content.cursor.point;
        let cursor_line = cursor_point.line.0 + offset as i32;
        let cursor = match content.cursor.shape {
            VShape::Hidden => None,
            _ if cursor_line < 0 || cursor_line as usize >= rows => None,
            shape => Some(Cursor {
                row: cursor_line as u16,
                col: cursor_point.column.0 as u16,
                shape: match shape {
                    VShape::Beam => CursorShape::Beam,
                    VShape::Underline => CursorShape::Underline,
                    VShape::HollowBlock => CursorShape::HollowBlock,
                    _ => CursorShape::Block,
                },
                wide: term.grid()[cursor_point].flags.contains(Flags::WIDE_CHAR),
            }),
        };

        let mut lines = vec![FLine::default(); rows];
        let mut links: Vec<String> = Vec::new();
        for indexed in content.display_iter {
            let point = indexed.point;
            let cell = indexed.cell;
            let row = point.line.0 + offset as i32;
            if row < 0 || row as usize >= rows {
                continue;
            }
            let line = &mut lines[row as usize];
            let col = point.column.0;
            if col + 1 == cols && cell.flags.contains(Flags::WRAPLINE) {
                line.wrapped = true;
            }
            if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            let mut f = map_flags(cell.flags);
            if selection.is_some_and(|s| s.contains(point)) {
                f |= flags::SELECTED;
            }
            let wide = cell.flags.contains(Flags::WIDE_CHAR);
            if wide {
                f |= flags::WIDE;
            }
            let link = match cell.hyperlink() {
                Some(h) => match links.iter().position(|l| l == h.uri()) {
                    Some(i) => i as u16 + 1,
                    None => {
                        links.push(h.uri().to_string());
                        links.len() as u16
                    }
                },
                None => 0,
            };
            let fg = map_color(cell.fg);
            let bg = map_color(cell.bg);
            let ch = if cell.flags.contains(Flags::HIDDEN) || cell.c == '\0' || cell.c == '\t' {
                ' '
            } else {
                cell.c
            };
            let zerowidth = cell.zerowidth();
            match line.runs.last_mut() {
                Some(r) if !wide
                    && r.flags == f
                    && r.fg == fg
                    && r.bg == bg
                    && r.link == link
                    && (r.col + r.width) as usize == col =>
                {
                    r.text.push(ch);
                    if let Some(zw) = zerowidth {
                        r.text.extend(zw.iter());
                    }
                    r.width += 1;
                }
                _ => {
                    let mut text = String::with_capacity(8);
                    text.push(ch);
                    if let Some(zw) = zerowidth {
                        text.extend(zw.iter());
                    }
                    line.runs.push(Run {
                        col: col as u16,
                        width: if wide { 2 } else { 1 },
                        text,
                        fg,
                        bg,
                        flags: f,
                        link,
                    });
                }
            }
        }

        const VISIBLE: u16 = flags::INVERSE
            | flags::SELECTED
            | flags::UNDERLINE
            | flags::DOUBLE_UNDERLINE
            | flags::UNDERCURL
            | flags::STRIKE;
        for line in &mut lines {
            line.runs.retain(|r| {
                r.bg != Color::Named(NAMED_BG) || r.flags & VISIBLE != 0 || r.text.chars().any(|c| c != ' ')
            });
        }

        Frame {
            id: self.id,
            cols: cols as u16,
            rows: rows as u16,
            display_offset: offset as u32,
            history: term.grid().history_size() as u32,
            mode: content.mode.bits(),
            cursor,
            lines,
            links,
        }
    }

    /// Serialize the primary screen + history as ANSI text for restoring after a restart.
    /// Returns `None` while a full-screen app (alternate screen) is running.
    pub fn dump_ansi(&self, max_lines: usize) -> Option<Vec<u8>> {
        let term = self.term.lock();
        if term.mode().contains(TermMode::ALT_SCREEN) {
            return None;
        }
        let grid = term.grid();
        let cols = grid.columns();
        let top = grid.topmost_line().0;
        let bottom = grid.bottommost_line().0;
        let start = top.max(bottom - max_lines as i32 + 1);

        let mut rendered: Vec<(String, bool)> = Vec::new();
        for l in start..=bottom {
            let row = &grid[Line(l)];
            let wrapped = row[Column(cols - 1)].flags.contains(Flags::WRAPLINE);
            let last = if wrapped {
                Some(cols - 1)
            } else {
                (0..cols).rev().find(|&c| {
                    let cell = &row[Column(c)];
                    cell.c != ' ' || cell.bg != VColor::Named(NamedColor::Background)
                })
            };
            let mut s = String::new();
            if let Some(last) = last {
                let mut current: Option<(VColor, VColor, Flags)> = None;
                for c in 0..=last {
                    let cell = &row[Column(c)];
                    if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                        continue;
                    }
                    let attr = (cell.fg, cell.bg, cell.flags & STYLE_FLAGS);
                    if current != Some(attr) {
                        write_sgr(&mut s, attr.0, attr.1, attr.2);
                        current = Some(attr);
                    }
                    s.push(if cell.c == '\0' || cell.c == '\t' { ' ' } else { cell.c });
                    if let Some(zw) = cell.zerowidth() {
                        s.extend(zw.iter());
                    }
                }
                if current.is_some() {
                    s.push_str("\x1b[0m");
                }
            }
            rendered.push((s, wrapped));
        }
        while rendered.last().is_some_and(|(s, w)| s.is_empty() && !w) {
            rendered.pop();
        }
        let mut out = String::new();
        for (s, wrapped) in rendered {
            out.push_str(&s);
            if !wrapped {
                out.push_str("\r\n");
            }
        }
        Some(out.into_bytes())
    }
}

const STYLE_FLAGS: Flags = Flags::BOLD
    .union(Flags::ITALIC)
    .union(Flags::UNDERLINE)
    .union(Flags::DIM)
    .union(Flags::INVERSE)
    .union(Flags::HIDDEN)
    .union(Flags::STRIKEOUT);

fn write_sgr(s: &mut String, fg: VColor, bg: VColor, f: Flags) {
    s.push_str("\x1b[0");
    for (flag, code) in [
        (Flags::BOLD, 1),
        (Flags::DIM, 2),
        (Flags::ITALIC, 3),
        (Flags::UNDERLINE, 4),
        (Flags::INVERSE, 7),
        (Flags::HIDDEN, 8),
        (Flags::STRIKEOUT, 9),
    ] {
        if f.contains(flag) {
            let _ = write!(s, ";{code}");
        }
    }
    let mut color = |c: VColor, base: usize, bright: usize, ext: usize| match c {
        VColor::Named(n) => {
            let n = n as usize;
            if n < 8 {
                let _ = write!(s, ";{}", base + n);
            } else if n < 16 {
                let _ = write!(s, ";{}", bright + n - 8);
            }
        }
        VColor::Indexed(i) => {
            let _ = write!(s, ";{ext};5;{i}");
        }
        VColor::Spec(rgb) => {
            let _ = write!(s, ";{ext};2;{};{};{}", rgb.r, rgb.g, rgb.b);
        }
    };
    color(fg, 30, 90, 38);
    color(bg, 40, 100, 48);
    s.push('m');
}

fn map_flags(f: Flags) -> u16 {
    let mut out = 0;
    for (from, to) in [
        (Flags::BOLD, flags::BOLD),
        (Flags::ITALIC, flags::ITALIC),
        (Flags::UNDERLINE, flags::UNDERLINE),
        (Flags::DOUBLE_UNDERLINE, flags::DOUBLE_UNDERLINE),
        (Flags::UNDERCURL, flags::UNDERCURL),
        (Flags::DOTTED_UNDERLINE, flags::UNDERLINE),
        (Flags::DASHED_UNDERLINE, flags::UNDERLINE),
        (Flags::STRIKEOUT, flags::STRIKE),
        (Flags::DIM, flags::DIM),
        (Flags::INVERSE, flags::INVERSE),
        (Flags::HIDDEN, flags::HIDDEN),
    ] {
        if f.contains(from) {
            out |= to;
        }
    }
    out
}

fn map_color(c: VColor) -> Color {
    match c {
        VColor::Named(n) => Color::Named(n as u16),
        VColor::Indexed(i) => Color::Indexed(i),
        VColor::Spec(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
    }
}

fn build_regex(query: &SearchQuery) -> Option<RegexSearch> {
    if query.text.is_empty() {
        return None;
    }
    let mut pattern = if query.regex { query.text.clone() } else { escape_regex(&query.text) };
    if query.case_sensitive {
        pattern = format!("(?-i){pattern}");
    }
    RegexSearch::new(&pattern).ok()
}

fn escape_regex(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 2);
    for c in text.chars() {
        if "\\.+*?()|[]{}^$#&-~".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_states() {
        let screen = |s: &str| s.lines().map(String::from).collect::<Vec<_>>();
        let idle = screen(" ▐▛███▛█   Claude Code v2.1.312\n──────────\n❯ Try \"fix lint errors\"\n──────────\n  ⏵⏵ auto mode on");
        assert_eq!(detect_claude(&idle), CLAUDE_READY);
        let working = screen("⏺ Reading files\n✻ Thinking… (12s · esc to interrupt)\n──────────\n❯ \n──────────");
        assert_eq!(detect_claude(&working), CLAUDE_WORKING);
        // A narrow pane cuts the hint; the spinner line still gives it away.
        let narrow = screen("✻ Sketching… (20s · ↓ 1.1k tokens)\n──────────\n❯\n──────────\n  ⏵⏵ auto mode on (shift+tab to cycle) · esc to int…");
        assert_eq!(detect_claude(&narrow), CLAUDE_WORKING);
        let narrow_no_hint = screen("✻ Sketching… (20s · ↓ 1.1k tokens)\n──────────\n❯\n──────────");
        assert_eq!(detect_claude(&narrow_no_hint), CLAUDE_WORKING);
        let asking = screen(" Do you want to make this edit to main.rs?\n ❯ 1. Yes\n   2. Yes, and don't ask again\n   3. No");
        assert_eq!(detect_claude(&asking), CLAUDE_NEEDS_YOU);
        assert_eq!(detect_claude(&screen("$ ls\nfoo bar")), CLAUDE_UNKNOWN);
    }

    #[test]
    fn escape() {
        assert_eq!(escape_regex("a.b*(c)"), "a\\.b\\*\\(c\\)");
    }
}

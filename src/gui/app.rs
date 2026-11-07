//! Application state and UI: sidebar with vertical tabs, pages, the tiling grid, overlays.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use egui::{
    pos2, vec2, Align, Align2, Color32, CornerRadius, CursorIcon, FontId, Id, Key, Layout, Pos2, Rect,
    RichText, Sense, Stroke, StrokeKind, Ui, UiBuilder,
};
use winit::event::{ElementState, KeyEvent};
use winit::event_loop::EventLoopProxy;
use winit::keyboard::ModifiersState;
use winit::window::{UserAttentionType, Window};

use crate::config::{Config, ImagePaste, NewSessionCwd, THEME_NAMES};
use crate::model::{Arrange, Dir, Edge, Node, SessionId, ViewMode, Workspace};
use crate::paths;
use crate::protocol::{
    mode, ClientMsg, Frame, ScrollCmd, SearchHit, SearchQuery, SelectCmd, SelectKind, ServerMsg, SessionInfo,
};

use super::client::{self, Conn, UserEvent};
use super::fonts;
use super::icons::{self, Icon};
use super::keys::{self, Action, Keymap};
use super::term_view::{self, mix, DrawOpts, GlyphCache, Metrics, Palette};

const TOPBAR_H: f32 = 36.0;
const PANE_TITLE_H: f32 = 22.0;
const TRAFFIC_LIGHTS_W: f32 = 76.0;
const BLINK: Duration = Duration::from_millis(530);

#[derive(Default)]
struct SessionView {
    info: SessionInfo,
    frame: Option<Box<Frame>>,
    sent_size: (u16, u16, u16, u16),
    activity: bool,
    bell: bool,
    scroll_acc: f32,
    mouse_down: Option<u8>,
    last_cell: Option<(u16, u16)>,
    claude: Option<ClaudeView>,
}

/// Claude Code running (or last run) in a terminal.
#[derive(Clone, Default)]
struct ClaudeView {
    /// Resume id.
    id: String,
    title: String,
    running: bool,
    /// 0 unknown, 1 ready, 2 working, 3 needs you.
    state: u8,
    /// Finished while nobody was looking.
    done: bool,
}

const CLAUDE_READY: u8 = 1;
const CLAUDE_WORKING: u8 = 2;
const CLAUDE_NEEDS_YOU: u8 = 3;
/// Closed to save memory; reopens when the terminal is viewed.
const CLAUDE_ASLEEP: u8 = 4;

struct ProfilePrompt {
    index: usize,
    labels: Vec<String>,
    values: Vec<String>,
    focus: bool,
}

struct History {
    /// The arrow keys were used, so show the keyboard cursor.
    keyboard: bool,
    req: u64,
    loading: bool,
    sessions: Vec<crate::claude::ClaudeSession>,
    query: String,
    selected: usize,
    focus: bool,
}

#[derive(Clone, Copy)]
/// Where a requested terminal goes. `page` is a page *id*: indices shift when pages are dropped.
enum Placement {
    Auto { page: u64 },
    Split { page: u64, dir: Dir },
}

struct FindBar {
    session: SessionId,
    query: String,
    regex: bool,
    case: bool,
    not_found: bool,
    focus: bool,
}

struct GlobalSearch {
    query: String,
    regex: bool,
    case: bool,
    req: u64,
    hits: Vec<SearchHit>,
    searching: bool,
    selected: usize,
    focus: bool,
    edited_at: Option<Instant>,
}

struct Switcher {
    query: String,
    selected: usize,
    focus: bool,
}

enum RenameTarget {
    Session(SessionId),
    /// A page id.
    Page(u64),
}

/// Where a name is being edited; a page shows in the sidebar and in the tabs at once,
/// and only the place that was double-clicked gets the editor.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RenamePlace {
    Sidebar,
    Tabs,
}

fn same_target(a: &RenameTarget, b: &RenameTarget) -> bool {
    match (a, b) {
        (RenameTarget::Session(x), RenameTarget::Session(y)) => x == y,
        (RenameTarget::Page(x), RenameTarget::Page(y)) => x == y,
        _ => false,
    }
}

struct Rename {
    place: RenamePlace,
    target: RenameTarget,
    text: String,
    focus: bool,
}

enum Confirm {
    Paste { session: SessionId, text: String },
    Close { sessions: Vec<SessionId>, what: String },
    KillAll,
    RestartDaemon,
}

struct Div {
    split: u64,
    index: usize,
    dir: Dir,
    rect: Rect,
    span: f32,
}

pub struct App {
    pub ctx: egui::Context,
    proxy: EventLoopProxy<UserEvent>,
    conn: Option<Conn>,
    conn_error: Option<String>,
    /// Local daemon, or a server reached over SSH.
    target: client::Target,
    /// Where a background reconnect leaves the new connection.
    pending_conn: Arc<std::sync::Mutex<Option<Conn>>>,
    reconnecting: bool,
    reconnect_attempts: u32,
    inbox: Receiver<ServerMsg>,
    inbox_tx: Sender<ServerMsg>,
    pending: Arc<AtomicBool>,

    pub config: Config,
    pal: Palette,
    keymap: Keymap,
    style_dirty: bool,
    fonts_dirty: bool,
    font_size: f32,
    metrics: Option<Metrics>,
    glyphs: GlyphCache,

    pub ws: Workspace,
    ws_dirty: bool,
    have_state: bool,
    auto_created: bool,
    sessions: HashMap<SessionId, SessionView>,

    pub modifiers: ModifiersState,
    subscribed: Vec<SessionId>,
    visible: Vec<SessionId>,
    pane_rects: Vec<(SessionId, Rect)>,
    central_rect: Rect,
    next_req: u64,
    creates: HashMap<u64, Placement>,
    copy_reqs: HashSet<u64>,

    drag_pane: Option<SessionId>,
    drop_target: Option<(SessionId, Edge)>,
    page_drop: Option<usize>,
    hover_link: Option<(SessionId, String, Vec<(u16, u16, u16)>)>,
    autoscroll: bool,

    /// Multi-select mode in the sidebar (entered by press-and-hold or ⌘-click).
    select: Option<HashSet<SessionId>>,
    /// The row whose long press started selection; its release must not toggle it again.
    long_press: Option<SessionId>,
    /// Pages collapsed in the sidebar (by page id).
    pub collapsed: HashSet<u64>,
    find: Option<FindBar>,
    search: Option<GlobalSearch>,
    switcher: Option<Switcher>,
    rename: Option<Rename>,
    confirm: Option<Confirm>,
    toast: Option<(String, Instant)>,
    filter: String,

    pub window: Option<Arc<Window>>,
    pub window_focused: bool,
    pub occluded: bool,
    pub dropped: Vec<PathBuf>,
    pub hovering_files: bool,
    clipboard: Option<arboard::Clipboard>,
    pub quit: bool,
    last_input: Instant,
    reported_focus: Option<(SessionId, bool)>,
    bell_flash: Option<(SessionId, Instant)>,
    last_title: String,
    /// When the oldest not-yet-drawn terminal frame arrived (for latency diagnostics).
    pub frame_arrived: Option<Instant>,
    /// Pid of a running daemon that speaks an older protocol.
    daemon_outdated: Option<u32>,
    daemon_pid: Option<u32>,
    profile_prompt: Option<ProfilePrompt>,
    history: Option<History>,
    /// Fullscreen with nothing but terminals.
    focus_mode: bool,
    /// "Connect to a server" dialog: (host, focus).
    server_prompt: Option<(String, bool)>,
    shortcuts: Option<ShortcutsView>,
    /// The inline rename editor was drawn this frame (otherwise the rename is dropped).
    rename_drawn: bool,
}

struct ShortcutsView {
    filter: String,
    /// Waiting for the keys of this action's new shortcut.
    capture: Option<Action>,
}

fn snap(p: Pos2, ppp: f32) -> Pos2 {
    pos2((p.x * ppp).round() / ppp, (p.y * ppp).round() / ppp)
}

fn now_millis() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}

fn layout_node(node: &Node, rect: Rect, gap: f32, panes: &mut Vec<(SessionId, Rect)>, divs: &mut Vec<Div>) {
    match node {
        Node::Leaf(id) => panes.push((*id, rect)),
        Node::Split { id, dir, children, ratios } => {
            let n = children.len();
            let horizontal = *dir == Dir::Horizontal;
            let total = (if horizontal { rect.width() } else { rect.height() }) - gap * (n.saturating_sub(1)) as f32;
            let mut pos = if horizontal { rect.left() } else { rect.top() };
            for (i, (child, ratio)) in children.iter().zip(ratios).enumerate() {
                let size = (total * ratio).max(0.0);
                let r = if horizontal {
                    Rect::from_min_size(pos2(pos, rect.top()), vec2(size, rect.height()))
                } else {
                    Rect::from_min_size(pos2(rect.left(), pos), vec2(rect.width(), size))
                };
                layout_node(child, r, gap, panes, divs);
                pos += size;
                if i + 1 < n {
                    let dr = if horizontal {
                        Rect::from_min_max(pos2(pos, rect.top()), pos2(pos + gap, rect.bottom())).expand2(vec2(3.0, 0.0))
                    } else {
                        Rect::from_min_max(pos2(rect.left(), pos), pos2(rect.right(), pos + gap)).expand2(vec2(0.0, 3.0))
                    };
                    divs.push(Div { split: *id, index: i, dir: *dir, rect: dr, span: total.max(1.0) });
                    pos += gap;
                }
            }
        }
    }
}

fn edge_for(rect: Rect, pos: Pos2) -> Edge {
    let rx = (pos.x - rect.left()) / rect.width();
    let ry = (pos.y - rect.top()) / rect.height();
    let candidates = [(rx, Edge::Left), (1.0 - rx, Edge::Right), (ry, Edge::Top), (1.0 - ry, Edge::Bottom)];
    let (d, edge) = candidates.into_iter().fold((f32::MAX, Edge::Center), |a, b| if b.0 < a.0 { b } else { a });
    if d > 0.28 {
        Edge::Center
    } else {
        edge
    }
}

fn edge_rect(rect: Rect, edge: Edge) -> Rect {
    let c = rect.center();
    match edge {
        Edge::Left => Rect::from_min_max(rect.min, pos2(c.x, rect.max.y)),
        Edge::Right => Rect::from_min_max(pos2(c.x, rect.min.y), rect.max),
        Edge::Top => Rect::from_min_max(rect.min, pos2(rect.max.x, c.y)),
        Edge::Bottom => Rect::from_min_max(pos2(rect.min.x, c.y), rect.max),
        Edge::Center => rect,
    }
}

const LONG_PRESS: f64 = 0.45;
/// Height of the strip listing hidden terminals under the grid.
const DOCK_H: f32 = 30.0;
/// Smallest pane a split may produce, in cells.
const MIN_COLS: f32 = 24.0;
const MIN_ROWS: f32 = 6.0;

/// Another window: on this machine, or attached to a server's daemon.
fn open_window(host: Option<&str>) -> io::Result<()> {
    let mut cmd = std::process::Command::new(std::env::current_exe()?);
    match host {
        Some(host) => cmd.args(["--remote", host]),
        None => cmd.arg("--window"),
    };
    crate::client::spawn_detached(&mut cmd)
}

/// Flat search field: filled background and a magnifier, no outline even when focused.
fn search_field(ui: &mut Ui, text: &mut String, hint: &str, pal: &Palette, width: f32) -> egui::Response {
    let (rect, _) = ui.allocate_exact_size(vec2(width, 40.0), Sense::hover());
    ui.painter().rect_filled(rect, 10.0, mix(mix(pal.panel, pal.fg, 0.07), pal.fg, 0.06));
    icons::paint(ui.painter(), Icon::Search, pos2(rect.left() + 18.0, rect.center().y), pal.dim);
    let edit = Rect::from_min_max(pos2(rect.left() + 34.0, rect.top()), pos2(rect.right() - 12.0, rect.bottom()));
    ui.put(
        edit,
        egui::TextEdit::singleline(text)
            .frame(false)
            .hint_text(hint)
            .font(egui::TextStyle::Body)
            .vertical_align(Align::Center),
    )
}

enum InlineEdit {
    Editing,
    Commit,
    Cancel,
}

/// Edit `text` in place inside `rect`. All text is selected when it opens; Enter or a click
/// elsewhere commits, Esc cancels.
#[allow(clippy::too_many_arguments)]
fn inline_edit(ui: &mut Ui, rect: Rect, id: Id, text: &mut String, focus: &mut bool, font: FontId, pal: &Palette) -> InlineEdit {
    ui.painter().rect_filled(rect, 6.0, mix(pal.panel, pal.fg, 0.14));
    let inner = rect.shrink2(vec2(8.0, 0.0));
    let out = ui
        .scope_builder(UiBuilder::new().max_rect(inner), |ui| {
            egui::TextEdit::singleline(text)
                .id(id)
                .frame(false)
                .font(font)
                .margin(egui::Margin::ZERO)
                .desired_width(inner.width())
                .min_size(inner.size())
                .vertical_align(Align::Center)
                .show(ui)
        })
        .inner;
    if *focus {
        out.response.request_focus();
        let mut state = out.state.clone();
        let end = egui::text::CCursor::new(text.chars().count());
        state.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), end)));
        state.store(ui.ctx(), id);
        *focus = false;
        return InlineEdit::Editing;
    }
    if out.response.lost_focus() {
        if ui.input(|i| i.key_pressed(Key::Escape)) {
            InlineEdit::Cancel
        } else {
            InlineEdit::Commit
        }
    } else {
        InlineEdit::Editing
    }
}

/// Flat text field (filled, no outline).
fn text_field(ui: &mut Ui, text: &mut String, hint: &str, width: f32, color: Option<Color32>, pal: &Palette) -> egui::Response {
    let (rect, _) = ui.allocate_exact_size(vec2(width, 38.0), Sense::hover());
    ui.painter().rect_filled(rect, 9.0, mix(mix(pal.panel, pal.fg, 0.07), pal.fg, 0.06));
    let mut edit = egui::TextEdit::singleline(text).frame(false).hint_text(hint).font(egui::TextStyle::Body).vertical_align(Align::Center);
    if let Some(c) = color {
        edit = edit.text_color(c);
    }
    ui.put(rect.shrink2(vec2(10.0, 0.0)), edit)
}

/// Height for a dialog's list: a good part of the window, so it is comfortable to scroll.
fn list_height(window_h: f32) -> f32 {
    (window_h * 0.6).clamp(220.0, 640.0)
}

/// Menu entry with its shortcut aligned on the right.
fn menu_item(ui: &mut Ui, text: &str, shortcut: String, pal: &Palette) -> egui::Response {
    ui.add(egui::Button::new(text).shortcut_text(RichText::new(shortcut).color(pal.dim)).min_size(vec2(240.0, 0.0)))
}

/// Flat button that only shows a background on hover.
fn icon_btn(ui: &mut Ui, icon: &str, size: f32, color: Color32, tip: impl Into<String>) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(icon).size(size).color(color))
            .frame(true)
            .frame_when_inactive(false)
            .min_size(vec2(26.0, 24.0)),
    )
    .on_hover_text(tip.into())
}

/// Claude status: yellow "!" when it needs you, animated dots while working, green check when done.
fn status_badge(ui: &Ui, painter: &egui::Painter, c: Pos2, state: u8, done: bool, pal: &Palette) {
    match state {
        CLAUDE_NEEDS_YOU => {
            painter.circle_filled(c, 7.5, pal.table[3]);
            icons::paint(painter, Icon::Exclamation, c, pal.bg);
        }
        CLAUDE_WORKING => {
            let phase = (ui.input(|i| i.time) * 3.0) as usize % 3;
            for i in 0..3 {
                let color = if i == phase { pal.accent } else { pal.accent.gamma_multiply(0.35) };
                painter.circle_filled(c + vec2((i as f32 - 1.0) * 5.0, 0.0), 2.0, color);
            }
            // Slow animation: a few frames per second only while something is working.
            ui.ctx().request_repaint_after(Duration::from_millis(330));
        }
        _ if done => {
            painter.circle_filled(c, 7.5, pal.table[2]);
            icons::paint(painter, Icon::Check, c, pal.bg);
        }
        CLAUDE_READY => {
            painter.circle_filled(c, 4.5, pal.table[2]);
        }
        CLAUDE_ASLEEP => icons::paint(painter, Icon::Moon, c, pal.dim),
        _ => {}
    }
}

fn checkbox(painter: &egui::Painter, center: Pos2, checked: bool, pal: &Palette) {
    if checked {
        painter.circle_filled(center, 8.0, pal.accent);
        icons::paint(painter, Icon::Check, center, pal.bg);
    } else {
        painter.circle_stroke(center, 7.5, Stroke::new(1.5, pal.dim));
    }
}

/// Subsequence match used by the quick switcher.
fn fuzzy(hay: &str, needle: &str) -> bool {
    let hay = hay.to_lowercase();
    let mut it = hay.chars();
    needle.to_lowercase().chars().filter(|c| !c.is_whitespace()).all(|n| it.any(|h| h == n))
}

fn truncated(painter: &egui::Painter, text: &str, font: FontId, color: Color32, max_w: f32) -> Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_string(), font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(max_w.max(10.0));
    painter.layout_job(job)
}

fn save_png(img: &arboard::ImageData<'_>) -> io::Result<PathBuf> {
    let dir = paths::paste_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("paste-{}.png", now_millis()));
    let file = std::fs::File::create(&path)?;
    let mut enc = png::Encoder::new(io::BufWriter::new(file), img.width as u32, img.height as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().map_err(io::Error::other)?;
    writer.write_image_data(&img.bytes).map_err(io::Error::other)?;
    Ok(path)
}

fn cleanup_pastes(days: u64) {
    let Ok(entries) = std::fs::read_dir(paths::paste_dir()) else { return };
    let max_age = Duration::from_secs(days.max(1) * 86_400);
    for entry in entries.flatten() {
        let old = entry.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok());
        if old.is_some_and(|age| age > max_age) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

impl App {
    pub fn new(ctx: egui::Context, proxy: EventLoopProxy<UserEvent>) -> App {
        let (config, err) = Config::load();
        let (keymap, mut errors) = Keymap::new(&config.keys, &config.profiles);
        errors.extend(err);
        let (inbox_tx, inbox) = mpsc::channel();
        let pending = Arc::new(AtomicBool::new(false));
        let target = client::Target::from_args();
        let (conn, conn_error) = match client::connect(&target, proxy.clone(), inbox_tx.clone(), pending.clone()) {
            Ok(c) => (Some(c), None),
            Err(e) => (None, Some(format!("Cannot reach the sennya daemon: {e}"))),
        };
        cleanup_pastes(config.clipboard.keep_pastes_days);
        let pal = Palette::new(&config.theme());
        let font_size = config.font.size;
        let mode = match config.ui.default_mode {
            crate::config::ModeCfg::Tabs => ViewMode::Tabs,
            crate::config::ModeCfg::Grid => ViewMode::Grid,
        };
        App {
            ctx,
            proxy,
            conn,
            conn_error,
            target,
            pending_conn: Arc::new(std::sync::Mutex::new(None)),
            reconnecting: false,
            reconnect_attempts: 0,
            inbox,
            inbox_tx,
            pending,
            config,
            pal,
            keymap,
            style_dirty: true,
            fonts_dirty: true,
            font_size,
            metrics: None,
            glyphs: GlyphCache::default(),
            ws: Workspace::new(mode),
            ws_dirty: false,
            have_state: false,
            auto_created: false,
            sessions: HashMap::new(),
            modifiers: ModifiersState::empty(),
            subscribed: Vec::new(),
            visible: Vec::new(),
            pane_rects: Vec::new(),
            central_rect: Rect::from_min_size(Pos2::ZERO, vec2(900.0, 600.0)),
            next_req: 0,
            creates: HashMap::new(),
            copy_reqs: HashSet::new(),
            drag_pane: None,
            drop_target: None,
            page_drop: None,
            hover_link: None,
            autoscroll: false,
            select: None,
            long_press: None,
            collapsed: HashSet::new(),
            find: None,
            search: None,
            switcher: None,
            rename: None,
            confirm: None,
            toast: errors.first().map(|e| (e.clone(), Instant::now())),
            filter: String::new(),
            window: None,
            window_focused: true,
            occluded: false,
            dropped: Vec::new(),
            hovering_files: false,
            clipboard: None,
            quit: false,
            last_input: Instant::now(),
            reported_focus: None,
            bell_flash: None,
            last_title: String::new(),
            frame_arrived: None,
            daemon_outdated: None,
            daemon_pid: None,
            profile_prompt: None,
            history: None,
            focus_mode: false,
            server_prompt: None,
            shortcuts: None,
            rename_drawn: false,
        }
    }

    pub fn clear_color(&self) -> [f32; 4] {
        egui::Rgba::from(self.pal.bg).to_array()
    }

    fn send(&self, msg: ClientMsg) {
        if let Some(conn) = &self.conn {
            conn.send(&msg);
        }
    }

    fn req(&mut self) -> u64 {
        self.next_req += 1;
        self.next_req
    }

    fn toast(&mut self, text: impl Into<String>) {
        self.toast = Some((text.into(), Instant::now()));
    }

    fn clipboard(&mut self) -> Option<&mut arboard::Clipboard> {
        if self.clipboard.is_none() {
            self.clipboard = arboard::Clipboard::new().ok();
        }
        self.clipboard.as_mut()
    }

    /// Whether the visible part of a terminal has selected text.
    fn has_selection(&self, sid: SessionId) -> bool {
        let selected = |f: &Frame| f.lines.iter().any(|l| l.runs.iter().any(|r| r.flags & crate::protocol::flags::SELECTED != 0));
        self.sessions.get(&sid).and_then(|s| s.frame.as_deref()).is_some_and(selected)
    }

    fn mode_of(&self, sid: SessionId) -> u32 {
        self.sessions.get(&sid).and_then(|s| s.frame.as_ref()).map(|f| f.mode).unwrap_or(0)
    }

    // ---------------------------------------------------------------- events

    /// Returns true when a redraw is needed.
    pub fn on_user_event(&mut self, event: UserEvent) -> bool {
        match event {
            UserEvent::Wakeup => {
                self.pending.store(false, Ordering::Release);
                self.drain_inbox()
            }
            UserEvent::Disconnected => {
                self.conn = None;
                self.reconnect();
                true
            }
            UserEvent::ConfigChanged => {
                self.reload_config();
                true
            }
            UserEvent::Connected => {
                self.reconnecting = false;
                let conn = self.pending_conn.lock().unwrap().take();
                match conn {
                    Some(conn) => {
                        self.conn = Some(conn);
                        self.conn_error = None;
                        self.subscribed.clear();
                        for sv in self.sessions.values_mut() {
                            sv.sent_size = (0, 0, 0, 0);
                        }
                    }
                    None => self.reconnect(),
                }
                true
            }
            UserEvent::OutdatedDaemon(pid) => {
                // Never talk to it: it cannot read this version's messages.
                self.conn = None;
                self.daemon_outdated = Some(pid);
                true
            }
        }
    }

    /// Reconnect in the background with a growing pause, so a dead network never freezes the window.
    fn reconnect(&mut self) {
        if self.reconnecting {
            return;
        }
        self.reconnecting = true;
        let delay = Duration::from_millis((250u64 << self.reconnect_attempts.min(5)).min(8000));
        self.reconnect_attempts += 1;
        let target = self.target.clone();
        let (proxy, inbox, pending, slot) = (self.proxy.clone(), self.inbox_tx.clone(), self.pending.clone(), self.pending_conn.clone());
        std::thread::spawn(move || {
            std::thread::sleep(delay);
            if let Ok(conn) = client::connect(&target, proxy.clone(), inbox, pending) {
                *slot.lock().unwrap() = Some(conn);
            }
            let _ = proxy.send_event(UserEvent::Connected);
        });
    }

    pub fn reload_config(&mut self) {
        let (config, err) = Config::load();
        if let Some(err) = err {
            self.toast(err);
            return;
        }
        let (keymap, errors) = Keymap::new(&config.keys, &config.profiles);
        if let Some(e) = errors.first() {
            self.toast(e.clone());
        }
        if config.font.family != self.config.font.family {
            self.fonts_dirty = true;
        }
        if config.font.size != self.config.font.size {
            self.font_size = config.font.size;
        }
        self.metrics = None;
        self.pal = Palette::new(&config.theme());
        self.keymap = keymap;
        self.config = config;
        self.style_dirty = true;
        self.send(ClientMsg::ReloadConfig);
    }

    fn drain_inbox(&mut self) -> bool {
        let mut redraw = false;
        while let Ok(msg) = self.inbox.try_recv() {
            redraw = true;
            match msg {
                ServerMsg::Hello { version, pid } => {
                    self.daemon_pid = Some(pid);
                    self.daemon_outdated = (version < crate::protocol::MIN_DAEMON_VERSION).then_some(pid);
                }
                ServerMsg::FrameDelta(delta) => {
                    self.frame_arrived.get_or_insert_with(Instant::now);
                    match self.sessions.get_mut(&delta.id).and_then(|s| s.frame.as_mut()) {
                        Some(frame) => (*delta).apply(frame),
                        // No base to apply it to: subscribing again brings full frames.
                        None => self.subscribed.clear(),
                    }
                }
                ServerMsg::State { workspace, sessions } => {
                    self.ws = workspace;
                    let mut old = std::mem::take(&mut self.sessions);
                    for info in sessions {
                        let mut sv = old.remove(&info.id).unwrap_or_default();
                        sv.info = info;
                        self.sessions.insert(sv.info.id, sv);
                    }
                    self.have_state = true;
                    self.reconnect_attempts = 0;
                    if self.ws.all_sessions().is_empty() && !self.auto_created {
                        self.auto_created = true;
                        self.new_terminal(Placement::Auto { page: self.ws.page().id });
                    }
                }
                ServerMsg::SessionCreated { req, info } => {
                    let id = info.id;
                    self.sessions.insert(id, SessionView { info, ..Default::default() });
                    let placement = self.creates.remove(&req).unwrap_or(Placement::Auto { page: self.ws.page().id });
                    let mut hint = self.split_hint();
                    let index_of = |ws: &Workspace, id: u64| ws.pages.iter().position(|p| p.id == id).unwrap_or(ws.active_page);
                    match placement {
                        Placement::Auto { page } => {
                            let page = index_of(&self.ws, page);
                            // A manual page splits the focused pane; if it is too small, re-tile instead.
                            let manual = self.ws.pages.get(page).is_some_and(|p| p.arrange == Arrange::Manual && p.zone.is_none());
                            if manual && page == self.ws.active_page {
                                let focused = self.ws.focused();
                                let other = if hint == Dir::Horizontal { Dir::Vertical } else { Dir::Horizontal };
                                if !self.half_fits(focused, hint) {
                                    if self.half_fits(focused, other) {
                                        hint = other;
                                    } else {
                                        self.ws.set_arrange(page, Arrange::Grid);
                                    }
                                }
                            }
                            self.ws.add_session(id, page, None, hint)
                        }
                        Placement::Split { page, dir } => {
                            let page = index_of(&self.ws, page);
                            self.ws.add_session(id, page, Some(dir), hint)
                        }
                    }
                    self.ws_dirty = true;
                }
                ServerMsg::SessionUpdated(info) => {
                    if let Some(sv) = self.sessions.get_mut(&info.id) {
                        sv.info = info;
                    }
                }
                ServerMsg::SessionExited(id) => {
                    if matches!(&self.rename, Some(Rename { target: RenameTarget::Session(s), .. }) if *s == id) {
                        self.rename = None;
                    }
                    self.sessions.remove(&id);
                    if self.ws.page_of(id).is_some() {
                        self.ws.remove_session(id);
                        self.ws_dirty = true;
                    }
                    if self.find.as_ref().is_some_and(|f| f.session == id) {
                        self.find = None;
                    }
                }
                ServerMsg::Frame(frame) => {
                    self.frame_arrived.get_or_insert_with(Instant::now);
                    if let Some(sv) = self.sessions.get_mut(&frame.id) {
                        sv.frame = Some(frame);
                    }
                }
                ServerMsg::CopyText { req, text } => {
                    if self.copy_reqs.remove(&req) && !text.is_empty() {
                        if let Some(cb) = self.clipboard() {
                            let _ = cb.set_text(text);
                        }
                    }
                }
                ServerMsg::FindResult { id, found } => {
                    if let Some(f) = self.find.as_mut().filter(|f| f.session == id) {
                        f.not_found = !found;
                    }
                }
                ServerMsg::SearchResults { req, hits } => {
                    if let Some(s) = self.search.as_mut().filter(|s| s.req == req) {
                        s.hits = hits;
                        s.searching = false;
                        s.selected = 0;
                    }
                }
                ServerMsg::Activity(id) => {
                    if let Some(sv) = self.sessions.get_mut(&id) {
                        sv.activity = !self.visible.contains(&id);
                    }
                }
                ServerMsg::Bell(id) => {
                    if self.visible.contains(&id) {
                        self.bell_flash = Some((id, Instant::now()));
                    } else if let Some(sv) = self.sessions.get_mut(&id) {
                        sv.bell = true;
                    }
                    if !self.window_focused {
                        if let Some(w) = &self.window {
                            w.request_user_attention(Some(UserAttentionType::Informational));
                        }
                    }
                }
                ServerMsg::Clipboard(text) => {
                    if let Some(cb) = self.clipboard() {
                        let _ = cb.set_text(text);
                    }
                }
                ServerMsg::Error(err) => self.toast(err),
                ServerMsg::Ack { ok, message, .. } => {
                    if !ok {
                        self.toast(message);
                    }
                }
                ServerMsg::Text { .. } => {}
                ServerMsg::Layout { workspace, sessions } => {
                    let (page_id, mode, sidebar) = (self.ws.page().id, self.ws.mode, self.ws.sidebar);
                    let local: HashMap<u64, (Option<SessionId>, Option<SessionId>)> =
                        self.ws.pages.iter().map(|p| (p.id, (p.focused, p.zoomed))).collect();
                    self.ws = workspace;
                    for p in &mut self.ws.pages {
                        if let Some((focused, zoomed)) = local.get(&p.id) {
                            p.focused = focused.filter(|f| p.contains(*f)).or(p.focused);
                            p.zoomed = zoomed.filter(|z| p.contains(*z));
                        }
                    }
                    self.ws.active_page = self.ws.pages.iter().position(|p| p.id == page_id).unwrap_or(self.ws.active_page.min(self.ws.pages.len().saturating_sub(1)));
                    self.ws.mode = mode;
                    self.ws.sidebar = sidebar;
                    for info in sessions {
                        let id = info.id;
                        self.sessions.entry(id).or_default().info = info;
                    }
                }
                ServerMsg::Claude { id, session, title, running, state } => {
                    let seen = self.visible.contains(&id) && self.window_focused;
                    let mut notify = None;
                    if let Some(sv) = self.sessions.get_mut(&id) {
                        let mut c = sv.claude.take().unwrap_or_default();
                        let prev = c.state;
                        if prev == CLAUDE_WORKING && state == CLAUDE_READY && !seen {
                            c.done = true;
                            notify = Some("finished");
                        }
                        if state == CLAUDE_NEEDS_YOU && prev != CLAUDE_NEEDS_YOU && !seen {
                            notify = Some("needs your answer");
                        }
                        if state == CLAUDE_WORKING || !running {
                            c.done = false;
                        }
                        (c.id, c.title, c.running, c.state) = (session, title, running, state);
                        sv.claude = Some(c);
                    }
                    if let Some(what) = notify {
                        let name = self.title_of(id);
                        self.notify(&format!("Claude {what}"), &name);
                    }
                }
                ServerMsg::ClaudeHistory { req, sessions } => {
                    if let Some(h) = self.history.as_mut().filter(|h| h.req == req) {
                        h.sessions = sessions;
                        h.loading = false;
                        h.selected = 0;
                    }
                }
            }
        }
        redraw
    }

    /// Whether half of the pane showing `sid` still has room for a usable terminal.
    fn half_fits(&self, sid: Option<SessionId>, dir: Dir) -> bool {
        let (Some(m), Some(rect)) = (&self.metrics, self.pane_rects.iter().find(|(s, _)| Some(*s) == sid).map(|(_, r)| *r))
        else {
            return true;
        };
        match dir {
            Dir::Horizontal => rect.width() / 2.0 >= MIN_COLS * m.cell_w,
            Dir::Vertical => rect.height() / 2.0 >= MIN_ROWS * m.cell_h,
        }
    }

    fn split_hint(&self) -> Dir {
        let focused = self.ws.focused();
        let rect = self.pane_rects.iter().find(|(s, _)| Some(*s) == focused).map(|(_, r)| *r).unwrap_or(self.central_rect);
        if rect.width() > rect.height() * 1.25 {
            Dir::Horizontal
        } else {
            Dir::Vertical
        }
    }

    fn new_terminal(&mut self, placement: Placement) {
        let cwd = match self.config.terminal.new_session_cwd {
            NewSessionCwd::Inherit => self
                .ws
                .focused()
                .and_then(|s| self.sessions.get(&s))
                .map(|s| s.info.cwd.clone())
                .filter(|c| !c.is_empty()),
            NewSessionCwd::Home => None,
        };
        let (cols, rows) = match &self.metrics {
            Some(m) => m.grid_size(self.central_rect.size() * 0.5),
            None => (80, 24),
        };
        let req = self.req();
        self.creates.insert(req, placement);
        self.send(ClientMsg::CreateSession { req, cwd, cols, rows });
    }

    fn close_sessions(&mut self, sessions: &[SessionId]) {
        for &sid in sessions {
            self.send(ClientMsg::CloseSession(sid));
            self.sessions.remove(&sid);
            self.ws.remove_session(sid);
        }
        self.ws_dirty = true;
    }

    fn request_close(&mut self, sessions: Vec<SessionId>) {
        let busy: Vec<String> = sessions
            .iter()
            .filter_map(|s| self.sessions.get(s))
            .filter(|s| !s.info.process.is_empty())
            .map(|s| s.info.process.clone())
            .collect();
        if busy.is_empty() && sessions.len() <= 1 {
            self.close_sessions(&sessions);
        } else {
            let what = if busy.is_empty() {
                format!("Close {} terminals?", sessions.len())
            } else {
                format!("Still running: {}. Close anyway?", busy.join(", "))
            };
            self.confirm = Some(Confirm::Close { sessions, what });
        }
    }

    fn focus_session(&mut self, sid: SessionId) {
        let hidden = self.ws.page_of(sid).is_some_and(|p| self.ws.pages[p].hidden.contains(&sid));
        if hidden && self.ws.mode == ViewMode::Grid {
            self.ws.unhide(sid);
            self.ws_dirty = true;
        }
        if self.ws.focused() != Some(sid) || self.ws.page_of(sid) != Some(self.ws.active_page) {
            self.ws.focus(sid);
            self.ws_dirty = true;
        }
        if let Some(sv) = self.sessions.get_mut(&sid) {
            sv.activity = false;
            sv.bell = false;
        }
    }

    pub fn set_window_focus(&mut self, focused: bool) {
        self.window_focused = focused;
        self.last_input = Instant::now();
    }

    /// Keyboard input. Returns `None` to pass the event to egui, otherwise whether the UI
    /// needs a redraw. Plain typing does not: the terminal's echo arrives as a new frame,
    /// and drawing an unchanged frame first would only push that echo one vsync later.
    pub fn on_key(&mut self, event: &KeyEvent) -> Option<bool> {
        let egui_kb = self.ctx.wants_keyboard_input();
        if event.state != ElementState::Pressed {
            return (!egui_kb).then_some(false);
        }
        if let Some(action) = self.shortcuts.as_ref().and_then(|v| v.capture) {
            use winit::keyboard::{Key, NamedKey};
            let plain = self.modifiers.is_empty();
            match &event.logical_key {
                Key::Named(NamedKey::Escape) if plain => {}
                Key::Named(NamedKey::Backspace | NamedKey::Delete) if plain => self.set_shortcut(action, Some("none")),
                _ => match keys::Chord::from_event(event, self.modifiers) {
                    Some(chord) => self.set_shortcut(action, Some(&chord.to_config())),
                    None => return Some(false), // a lone modifier: keep waiting
                },
            }
            if let Some(v) = self.shortcuts.as_mut() {
                v.capture = None;
            }
            return Some(true);
        }
        if self.select.is_some() && event.logical_key == winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape) {
            self.select = None;
            return Some(true);
        }
        if let Some(action) = self.keymap.lookup(event, self.modifiers) {
            // A focused text field keeps its editing keys (Shift+Home, ⌘C...).
            let command = if cfg!(target_os = "macos") { self.modifiers.super_key() } else { self.modifiers.control_key() };
            if egui_kb && (!command || matches!(action, Action::Copy | Action::Paste | Action::SelectAll)) {
                return None;
            }
            // Plain Ctrl+C with nothing selected is the interrupt key, not Copy.
            let interrupt = action == Action::Copy
                && self.modifiers.control_key()
                && !self.modifiers.shift_key()
                && !self.ws.focused().is_some_and(|sid| self.has_selection(sid));
            if !interrupt {
                self.do_action(action);
                return Some(true);
            }
        }
        // G1: an inline rename only takes keys while its field really has focus (egui_kb).
        if egui_kb
            || self.confirm.is_some()
            || self.server_prompt.is_some()
            || self.switcher.is_some()
            || self.search.is_some()
            || self.history.is_some()
            || self.profile_prompt.is_some()
            || self.shortcuts.is_some()
        {
            return None;
        }
        let Some(sid) = self.ws.focused() else { return Some(false) };
        if self.ws.mode == ViewMode::Grid && self.ws.page().hidden.contains(&sid) {
            return Some(false);
        }
        let term_mode = self.mode_of(sid);
        if let Some(bytes) = keys::encode_key(event, self.modifiers, term_mode, &self.config.terminal) {
            self.send(ClientMsg::Input(sid, bytes));
            self.last_input = Instant::now();
        }
        // A blinking cursor restarts its phase on input, which is a visible change.
        Some(self.config.terminal.cursor_blink)
    }

    fn do_action(&mut self, action: Action) {
        let focused = self.ws.focused();
        let page = self.ws.active_page;
        let page_id = self.ws.page().id;
        match action {
            Action::NewTerminal => self.new_terminal(Placement::Auto { page: page_id }),
            Action::SplitRight | Action::SplitDown => {
                let wanted = if action == Action::SplitRight { Dir::Horizontal } else { Dir::Vertical };
                let other = if wanted == Dir::Horizontal { Dir::Vertical } else { Dir::Horizontal };
                let dir = if self.ws.mode == ViewMode::Tabs || self.half_fits(focused, wanted) {
                    wanted
                } else if self.half_fits(focused, other) {
                    self.toast(if wanted == Dir::Horizontal { "Too narrow to split right, split down instead" } else { "Too short to split down, split right instead" });
                    other
                } else {
                    let hint = format!("This pane is too small to split. Use a new page ({}) or rebalance ({})", self.keymap.label(Action::NewPage), self.keymap.label(Action::Rebalance));
                    self.toast(hint);
                    return;
                };
                self.ws.mode = ViewMode::Grid;
                self.ws_dirty = true;
                self.new_terminal(Placement::Split { page: page_id, dir });
            }
            Action::CloseTerminal => {
                if let Some(sid) = focused {
                    self.request_close(vec![sid]);
                }
            }
            Action::ToggleMode => {
                self.ws.mode = match self.ws.mode {
                    ViewMode::Tabs => ViewMode::Grid,
                    ViewMode::Grid => ViewMode::Tabs,
                };
                // Keep working in the same terminal even if it was hidden from the grid.
                if let (ViewMode::Grid, Some(f)) = (self.ws.mode, focused) {
                    if self.ws.page().hidden.contains(&f) {
                        self.ws.unhide(f);
                    }
                }
                self.ws_dirty = true;
            }
            Action::NewPage => {
                let idx = self.ws.new_page();
                self.ws_dirty = true;
                self.new_terminal(Placement::Auto { page: self.ws.pages[idx].id });
            }
            Action::ClosePage => {
                let sessions = self.ws.page().sessions();
                self.request_close(sessions);
            }
            Action::NextPage | Action::PrevPage => {
                let n = self.ws.pages.len();
                self.ws.active_page = if action == Action::NextPage { (page + 1) % n } else { (page + n - 1) % n };
                self.ws_dirty = true;
            }
            Action::GotoPage(i) => {
                if (i as usize) <= self.ws.pages.len() {
                    self.ws.active_page = i as usize - 1;
                    self.ws_dirty = true;
                }
            }
            Action::NextTerminal | Action::PrevTerminal => {
                let list = if self.ws.mode == ViewMode::Tabs { self.ws.all_sessions() } else { self.ws.page().tiled() };
                if let (Some(cur), false) = (focused, list.is_empty()) {
                    let i = list.iter().position(|s| *s == cur).unwrap_or(0);
                    let n = list.len();
                    let next = if action == Action::NextTerminal { (i + 1) % n } else { (i + n - 1) % n };
                    self.focus_session(list[next]);
                }
            }
            Action::ToggleSidebar => {
                self.ws.sidebar = !self.ws.sidebar;
                self.ws_dirty = true;
            }
            Action::Find => {
                if let Some(sid) = focused {
                    let query = self.find.take().map(|f| f.query).unwrap_or_default();
                    self.find = Some(FindBar { session: sid, query, regex: false, case: false, not_found: false, focus: true });
                }
            }
            Action::GlobalSearch => {
                self.search = Some(GlobalSearch {
                    query: String::new(),
                    regex: false,
                    case: false,
                    req: 0,
                    hits: Vec::new(),
                    searching: false,
                    selected: 0,
                    focus: true,
                    edited_at: None,
                });
            }
            Action::QuickSwitch => self.switcher = Some(Switcher { query: String::new(), selected: 0, focus: true }),
            Action::Copy => {
                if let Some(sid) = focused {
                    let req = self.req();
                    self.copy_reqs.insert(req);
                    self.send(ClientMsg::Copy { req, id: sid });
                }
            }
            Action::Paste => self.paste(),
            Action::SelectAll => {
                if let Some(sid) = focused {
                    self.send(ClientMsg::Select(sid, SelectCmd::All));
                }
            }
            Action::ClearScrollback => {
                if let Some(sid) = focused {
                    self.send(ClientMsg::ClearScrollback(sid));
                }
            }
            Action::FontBigger | Action::FontSmaller | Action::FontReset => {
                self.font_size = match action {
                    Action::FontBigger => (self.font_size + 1.0).min(48.0),
                    Action::FontSmaller => (self.font_size - 1.0).max(7.0),
                    _ => self.config.font.size,
                };
                self.metrics = None;
            }
            Action::ZoomPane => {
                if let Some(sid) = focused {
                    let p = self.ws.page_mut();
                    p.zoomed = if p.zoomed == Some(sid) { None } else { Some(sid) };
                    self.ws.mode = ViewMode::Grid;
                    self.ws_dirty = true;
                }
            }
            Action::Rebalance => {
                let arrange = match self.ws.page().arrange {
                    Arrange::Manual => Arrange::Grid,
                    other => other,
                };
                self.ws.set_arrange(page, arrange);
                self.ws_dirty = true;
            }
            Action::FocusLeft | Action::FocusRight | Action::FocusUp | Action::FocusDown => self.focus_dir(action),
            Action::OpenConfig => {
                let path = paths::config_file();
                #[cfg(target_os = "macos")]
                let ok = crate::client::spawn_detached(std::process::Command::new("open").arg("-t").arg(&path)).is_ok();
                #[cfg(windows)]
                let ok = crate::client::spawn_detached(std::process::Command::new("notepad").arg(&path)).is_ok();
                #[cfg(not(any(target_os = "macos", windows)))]
                let ok = crate::client::spawn_detached(std::process::Command::new("xdg-open").arg(&path)).is_ok();
                if !ok {
                    self.toast(format!("Config: {}", path.display()));
                }
            }
            Action::Rename => {
                if !self.ws.sidebar {
                    // The name is edited in place in the sidebar.
                    self.ws.sidebar = true;
                    self.ws_dirty = true;
                }
                if let Some(sid) = focused {
                    let text = self.ws.names.get(&sid).cloned().unwrap_or_else(|| self.title_of(sid));
                    self.start_rename(Rename { place: RenamePlace::Sidebar, target: RenameTarget::Session(sid), text, focus: true });
                }
            }
            Action::ScrollPageUp | Action::ScrollPageDown | Action::ScrollTop | Action::ScrollBottom => {
                if let Some(sid) = focused {
                    let cmd = match action {
                        Action::ScrollPageUp => ScrollCmd::PageUp,
                        Action::ScrollPageDown => ScrollCmd::PageDown,
                        Action::ScrollTop => ScrollCmd::Top,
                        _ => ScrollCmd::Bottom,
                    };
                    self.send(ClientMsg::Scroll(sid, cmd));
                }
            }
            Action::Quit => self.quit = true,
            Action::ClaudeHistory => self.open_history(),
            Action::NewWindow => {
                if let Err(err) = open_window(self.target.host()) {
                    self.toast(format!("Could not open a window: {err}"));
                }
            }
            Action::FocusMode => self.set_focus_mode(!self.focus_mode),
            Action::HidePane => {
                if let Some(sid) = focused {
                    self.ws.hide(sid);
                    self.ws.mode = ViewMode::Grid;
                    self.ws_dirty = true;
                    let hint = format!("Hidden, still running. Click it in the bar below to bring it back");
                    self.toast(hint);
                }
            }
            Action::Profile(i) => self.start_profile(i as usize),
        }
    }

    fn set_focus_mode(&mut self, on: bool) {
        self.focus_mode = on;
        if let Some(w) = &self.window {
            w.set_fullscreen(on.then_some(winit::window::Fullscreen::Borderless(None)));
        }
        if on {
            let hint = format!("Focus mode: {} to leave", self.keymap.label(Action::FocusMode));
            self.toast(hint);
        }
    }

    /// Claude's (state, finished-unseen) in a terminal where it is running.
    fn claude_status(&self, sid: SessionId) -> Option<(u8, bool)> {
        let c = self.sessions.get(&sid)?.claude.as_ref()?;
        (c.running || c.state == CLAUDE_ASLEEP).then_some((c.state, c.done))
    }

    /// The most important Claude status among `sids`: needs you > working > done > ready.
    fn page_claude_status(&self, sids: &[SessionId]) -> Option<(u8, bool)> {
        let all: Vec<(u8, bool)> = sids.iter().filter_map(|s| self.claude_status(*s)).collect();
        all.iter()
            .find(|(st, _)| *st == CLAUDE_NEEDS_YOU)
            .or_else(|| all.iter().find(|(st, _)| *st == CLAUDE_WORKING))
            .or_else(|| all.iter().find(|(_, done)| *done))
            .or_else(|| all.iter().find(|(st, _)| *st == CLAUDE_READY))
            .or_else(|| all.iter().find(|(st, _)| *st == CLAUDE_ASLEEP))
            .copied()
    }

    /// A system notification (and a dock bounce) while the window is in the background.
    fn notify(&self, title: &str, body: &str) {
        // Elsewhere the taskbar flash below is the notification.
        #[cfg(not(target_os = "macos"))]
        let _ = (title, body);
        if self.window_focused || !self.config.ui.notifications {
            return;
        }
        if let Some(w) = &self.window {
            w.request_user_attention(Some(UserAttentionType::Informational));
        }
        #[cfg(target_os = "macos")]
        {
            let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
            let script = format!("display notification \"{}\" with title \"sennya\" subtitle \"{}\"", esc(body), esc(title));
            let _ = crate::client::spawn_detached(std::process::Command::new("osascript").arg("-e").arg(script));
        }
    }

    /// Save a shortcut to `[keys]` (`None` restores the default) and apply it right away.
    fn set_shortcut(&mut self, action: Action, chord: Option<&str>) {
        match crate::config::set_config_value("keys", &action.name(), chord) {
            Ok(()) => self.reload_config(),
            Err(err) => self.toast(format!("Could not write the config: {err}")),
        }
    }

    fn shortcuts_overlay(&mut self, ctx: &egui::Context) {
        let Some(mut v) = self.shortcuts.take() else { return };
        let mut open = true;
        let pal = self.pal.clone();
        let mut start_capture: Option<Action> = None;
        let mut reset: Option<Action> = None;
        let list_h = list_height(self.central_rect.height() + TOPBAR_H);
        let actions: Vec<Action> = Action::all()
            .into_iter()
            .filter(|a| fuzzy(&a.title(), &v.filter) || fuzzy(&self.keymap.label(*a), &v.filter))
            .collect();
        let modal = self.modal("shortcuts").show(ctx, |ui| {
            ui.set_width(620.0);
            ui.label(RichText::new("Keyboard shortcuts").size(17.0).strong());
            ui.add_space(2.0);
            ui.label(RichText::new("Click a shortcut and press new keys. Esc cancels, ⌫ removes it.").color(pal.dim));
            ui.add_space(10.0);
            search_field(ui, &mut v.filter, "Filter actions or keys", &pal, 620.0);
            ui.add_space(6.0);
            egui::ScrollArea::vertical().min_scrolled_height(list_h).max_height(list_h).auto_shrink([false, false]).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                for action in &actions {
                    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 38.0), Sense::hover());
                    ui.painter().text(pos2(row.left() + 8.0, row.center().y), Align2::LEFT_CENTER, action.title(), self.font(self.ui_size()), pal.text);
                    let capturing = v.capture == Some(*action);
                    let chords = self.keymap.chords(*action);
                    let label = if capturing {
                        "Press keys…".to_string()
                    } else if chords.is_empty() {
                        "—".to_string()
                    } else {
                        chords.iter().map(|c| c.label()).collect::<Vec<_>>().join("  ")
                    };
                    let conflict: Vec<String> = chords.iter().flat_map(|c| self.keymap.users(c, *action)).map(|a| a.title()).collect();
                    let font = self.font(self.ui_size());
                    let text_w = ui.fonts_mut(|f| f.layout_no_wrap(label.clone(), font.clone(), pal.text).size().x);
                    let pill = Rect::from_min_size(pos2(row.right() - 50.0 - text_w - 24.0, row.top() + 4.0), vec2(text_w + 24.0, row.height() - 8.0));
                    let resp = ui.interact(pill, Id::new(("shortcut", action.name())), Sense::click());
                    let fill = if capturing {
                        mix(pal.panel, pal.accent, 0.35)
                    } else if resp.hovered() {
                        mix(pal.panel, pal.fg, 0.16)
                    } else {
                        mix(pal.panel, pal.fg, 0.09)
                    };
                    ui.painter().rect_filled(pill, 7.0, fill);
                    let color = if !conflict.is_empty() { pal.danger } else if capturing { pal.text } else { pal.text };
                    ui.painter().text(pill.center(), Align2::CENTER_CENTER, &label, font, color);
                    let tip = if conflict.is_empty() { "Click to change".to_string() } else { format!("Also used by: {}", conflict.join(", ")) };
                    if resp.on_hover_text(tip).clicked() {
                        start_capture = Some(*action);
                    }
                    if self.config.keys.contains_key(&action.name()) {
                        let r = Rect::from_center_size(pos2(row.right() - 22.0, row.center().y), vec2(42.0, 24.0));
                        let resp = ui.interact(r, Id::new(("shortcut-reset", action.name())), Sense::click());
                        if resp.hovered() {
                            ui.painter().rect_filled(r, 6.0, pal.panel_hover);
                        }
                        ui.painter().text(r.center(), Align2::CENTER_CENTER, "Reset", self.font(self.ui_size() - 3.0), if resp.hovered() { pal.text } else { pal.dim });
                        if resp.on_hover_text("Back to the default").clicked() {
                            reset = Some(*action);
                        }
                    }
                }
            });
        });
        if modal.should_close() && v.capture.is_none() {
            open = false;
        }
        if let Some(a) = start_capture {
            v.capture = Some(a);
        }
        if open {
            self.shortcuts = Some(v);
        }
        if let Some(a) = reset {
            self.set_shortcut(a, None);
        }
    }

    fn apply_rename(&mut self, target: RenameTarget, text: &str) {
        let text = text.trim().to_string();
        match target {
            RenameTarget::Session(sid) => {
                if text.is_empty() {
                    self.ws.names.remove(&sid);
                } else {
                    self.ws.names.insert(sid, text);
                }
            }
            RenameTarget::Page(id) => {
                if let Some(p) = self.ws.pages.iter_mut().find(|p| p.id == id).filter(|_| !text.is_empty()) {
                    p.name = text;
                }
            }
        }
        self.ws_dirty = true;
    }

    /// Show the inline name editor for `target` (if it is the one being renamed) inside `rect`.
    /// Returns true while editing, so the caller skips drawing the plain name.
    /// A rename starts after its field was laid out this frame (a double-click on it), so it
    /// counts as drawn until the next frame shows whether the field is on screen.
    fn start_rename(&mut self, rename: Rename) {
        self.rename = Some(rename);
        self.rename_drawn = true;
    }

    fn inline_rename(&mut self, ui: &mut Ui, rect: Rect, target: &RenameTarget, place: RenamePlace) -> bool {
        self.rename_drawn |= self.rename.as_ref().is_some_and(|r| r.place == place && same_target(&r.target, target));
        let same_place = self.rename.as_ref().is_some_and(|r| r.place == place);
        let editing = same_place
            && (matches!((&self.rename, target), (Some(Rename { target: RenameTarget::Session(a), .. }), RenameTarget::Session(b)) if a == b)
                || matches!((&self.rename, target), (Some(Rename { target: RenameTarget::Page(a), .. }), RenameTarget::Page(b)) if a == b));
        if !editing {
            return false;
        }
        let Some(mut r) = self.rename.take() else { return false };
        let font = self.font(self.ui_size());
        let pal = self.pal.clone();
        match inline_edit(ui, rect, Id::new("inline-rename"), &mut r.text, &mut r.focus, font, &pal) {
            InlineEdit::Editing => self.rename = Some(r),
            InlineEdit::Commit => self.apply_rename(r.target, &r.text),
            InlineEdit::Cancel => {}
        }
        true
    }

    /// Room for the macOS window buttons, which only exist in a normal (not fullscreen) window.
    fn traffic_lights_w(&self) -> f32 {
        let fullscreen = self.window.as_ref().is_some_and(|w| w.fullscreen().is_some());
        if cfg!(target_os = "macos") && !fullscreen {
            TRAFFIC_LIGHTS_W
        } else {
            0.0
        }
    }

    fn mono_ui(&self) -> bool {
        !self.config.ui.font.eq_ignore_ascii_case("system")
    }

    /// The interface font at `size`.
    fn font(&self, size: f32) -> FontId {
        if self.mono_ui() {
            FontId::monospace(size)
        } else {
            FontId::proportional(size)
        }
    }

    /// Interface text size from the config.
    fn ui_size(&self) -> f32 {
        self.config.ui.ui_font_size.max(13.0)
    }

    fn needs_new_daemon(&mut self) -> bool {
        if self.daemon_outdated.is_some() {
            self.toast("This needs the updated session daemon: Settings → Restart session daemon");
            return true;
        }
        false
    }

    fn start_profile(&mut self, index: usize) {
        if self.needs_new_daemon() {
            return;
        }
        let Some(profile) = self.config.profiles.get(index) else { return };
        let labels = crate::template::prompts(profile.texts());
        if labels.is_empty() {
            self.send_profile(index, Vec::new());
        } else {
            let values = vec![String::new(); labels.len()];
            self.profile_prompt = Some(ProfilePrompt { index, labels, values, focus: true });
        }
    }

    fn send_profile(&mut self, index: usize, mut vars: Vec<(String, String)>) {
        let Some(name) = self.config.profiles.get(index).map(|p| p.name.clone()) else { return };
        if let Some(cwd) = self.ws.focused().and_then(|s| self.sessions.get(&s)).map(|s| s.info.cwd.clone()) {
            vars.push(("cwd".into(), cwd));
        }
        let req = self.req();
        self.send(ClientMsg::RunProfile { req, name, vars, near: None });
    }

    fn open_history(&mut self) {
        if self.needs_new_daemon() {
            return;
        }
        let req = self.req();
        self.send(ClientMsg::ClaudeHistory { req, limit: 400 });
        self.history = Some(History { keyboard: false, req, loading: true, sessions: Vec::new(), query: String::new(), selected: 0, focus: true });
    }

    fn resume_claude(&mut self, cs: &crate::claude::ClaudeSession) {
        let spec = crate::protocol::SpawnSpec {
            cwd: Some(cs.cwd.clone()),
            open: "tab".into(),
            near: None,
            page: None,
            title: Some(cs.title.chars().take(40).collect()),
            run: vec![format!("claude --resume {}", cs.id)],
            prefill: None,
            prefill_after: None,
        };
        let req = self.req();
        self.send(ClientMsg::Spawn { req, spec });
    }

    /// Stop the daemon gracefully; it saves everything and the reconnect starts the new one,
    /// which brings the terminals back in their folders with their history.
    fn restart_daemon(&mut self) {
        if let Some(pid) = self.daemon_outdated.take() {
            let _ = crate::client::stop_daemon_for_restart(pid);
            // It saves everything and removes its socket; then the new daemon restores it.
            for _ in 0..100 {
                if !crate::sys::process_alive(pid) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            self.have_state = false;
            self.reconnect();
            self.toast("Session daemon updated");
        }
    }

    fn focus_dir(&mut self, action: Action) {
        let Some(cur) = self.ws.focused() else { return };
        let Some(&(_, cur_rect)) = self.pane_rects.iter().find(|(s, _)| *s == cur) else { return };
        let c = cur_rect.center();
        let best = self
            .pane_rects
            .iter()
            .filter(|(s, _)| *s != cur)
            .filter(|(_, r)| match action {
                Action::FocusLeft => r.max.x <= cur_rect.min.x + 1.0,
                Action::FocusRight => r.min.x >= cur_rect.max.x - 1.0,
                Action::FocusUp => r.max.y <= cur_rect.min.y + 1.0,
                _ => r.min.y >= cur_rect.max.y - 1.0,
            })
            .min_by(|(_, a), (_, b)| {
                let da = a.center().distance(c);
                let db = b.center().distance(c);
                da.total_cmp(&db)
            })
            .map(|(s, _)| *s);
        if let Some(s) = best {
            self.focus_session(s);
        }
    }

    fn paste(&mut self) {
        let Some(sid) = self.ws.focused() else { return };
        let image_mode = self.config.clipboard.image_paste;
        let Some(cb) = self.clipboard() else { return };
        if let Ok(files) = cb.get().file_list() {
            if !files.is_empty() {
                let text = files.iter().map(|p| keys::shell_escape(&p.to_string_lossy())).collect::<Vec<_>>().join(" ");
                self.paste_text(sid, text + " ");
                return;
            }
        }
        if let Ok(text) = cb.get_text() {
            if !text.is_empty() {
                self.paste_text(sid, text);
                return;
            }
        }
        match cb.get_image() {
            Ok(img) => match image_mode {
                ImagePaste::CtrlV => self.send(ClientMsg::Input(sid, vec![0x16])),
                ImagePaste::Path => match save_png(&img) {
                    Ok(path) => {
                        let text = keys::shell_escape(&path.to_string_lossy()) + " ";
                        self.send(ClientMsg::Input(sid, text.into_bytes()));
                        self.toast("Image pasted as file path");
                    }
                    Err(err) => self.toast(format!("Could not save the image: {err}")),
                },
            },
            Err(_) => {}
        }
    }

    fn paste_text(&mut self, sid: SessionId, text: String) {
        let term_mode = self.mode_of(sid);
        let bracketed = term_mode & mode::BRACKETED_PASTE != 0;
        if !bracketed && self.config.security.confirm_multiline_paste && text.trim_end().contains('\n') {
            self.confirm = Some(Confirm::Paste { session: sid, text });
            return;
        }
        self.send(ClientMsg::Input(sid, keys::paste_bytes(&text, term_mode)));
        self.last_input = Instant::now();
    }

    pub fn on_close(&mut self) {
        self.send(ClientMsg::Subscribe(Vec::new()));
        if self.ws_dirty {
            self.send(ClientMsg::SetWorkspace(self.ws.clone()));
        }
    }

    pub fn on_occluded(&mut self, occluded: bool) {
        self.occluded = occluded;
        if occluded {
            self.subscribed.clear();
            self.send(ClientMsg::Subscribe(Vec::new()));
        }
    }

    /// How long until something on screen changes by itself.
    pub fn next_wakeup(&self) -> Duration {
        let mut d = Duration::MAX;
        if self.config.terminal.cursor_blink && self.window_focused {
            let since = self.last_input.elapsed().as_millis() % BLINK.as_millis();
            d = d.min(BLINK - Duration::from_millis(since as u64));
        }
        if let Some((_, at)) = &self.toast {
            if let Some(left) = Duration::from_millis(2500).checked_sub(at.elapsed()) {
                d = d.min(left + Duration::from_millis(1));
            }
        }
        if let Some((_, at)) = &self.bell_flash {
            // One more frame right after the flash ends to erase it, then nothing.
            if let Some(left) = Duration::from_millis(220).checked_sub(at.elapsed()) {
                d = d.min(left + Duration::from_millis(1));
            }
        }
        if self.search.as_ref().is_some_and(|s| s.edited_at.is_some()) || self.autoscroll {
            d = d.min(Duration::from_millis(60));
        }
        d
    }

    fn title_of(&self, sid: SessionId) -> String {
        if let Some(name) = self.ws.names.get(&sid) {
            return name.clone();
        }
        let Some(sv) = self.sessions.get(&sid) else { return "terminal".into() };
        // Claude Code puts its current activity into the window title; show the chat's own
        // title instead and leave the state to the status line below it.
        if let Some(c) = sv.claude.as_ref().filter(|c| c.running || c.state == CLAUDE_ASLEEP) {
            return if c.title.is_empty() { "✳ Claude Code".into() } else { format!("✳ {}", c.title) };
        }
        let osc = sv.info.osc_title.trim();
        if !osc.is_empty() {
            return osc.to_string();
        }
        if !sv.info.process.is_empty() {
            return sv.info.process.clone();
        }
        let cwd = paths::tilde(&sv.info.cwd);
        match cwd.rsplit('/').next() {
            Some(last) if !last.is_empty() => last.to_string(),
            _ => cwd,
        }
    }

    /// The line under a terminal's name: just its folder.
    fn subtitle_of(&self, sid: SessionId) -> String {
        self.sessions.get(&sid).map(|sv| paths::tilde(&sv.info.cwd)).unwrap_or_default()
    }

    // ---------------------------------------------------------------- frame

    pub fn frame(&mut self, ctx: &egui::Context) {
        self.drain_inbox();
        if self.fonts_dirty {
            fonts::install(ctx, &self.config.font.family);
            self.fonts_dirty = false;
            self.metrics = None;
            self.glyphs.clear();
        }
        // Fonts installed this frame only apply from the next one, so compare against the
        // font egui actually uses now; a stale cell width makes text drift away from the grid.
        let stale = self.metrics.as_ref().is_none_or(|m| {
            m.ppp != ctx.pixels_per_point()
                || m.size != self.font_size
                || (ctx.fonts_mut(|f| f.glyph_width(&m.font, 'M')) - m.cell_w).abs() > 0.001
        });
        if stale {
            self.metrics = Some(Metrics::new(ctx, self.font_size, self.config.font.line_height));
            self.glyphs.clear();
        }
        if self.style_dirty {
            self.apply_style(ctx);
            self.style_dirty = false;
        }
        if !self.dropped.is_empty() {
            let files = std::mem::take(&mut self.dropped);
            let target = ctx
                .pointer_hover_pos()
                .and_then(|p| self.pane_rects.iter().find(|(_, r)| r.contains(p)).map(|(s, _)| *s))
                .or(self.ws.focused());
            if let Some(sid) = target {
                let text = files.iter().map(|p| keys::shell_escape(&p.to_string_lossy())).collect::<Vec<_>>().join(" ");
                self.focus_session(sid);
                self.send(ClientMsg::Input(sid, (text + " ").into_bytes()));
            }
            self.hovering_files = false;
        }
        if self.toast.as_ref().is_some_and(|(_, at)| at.elapsed() >= Duration::from_millis(2500)) {
            self.toast = None;
        }
        if self.bell_flash.is_some_and(|(_, at)| at.elapsed() >= Duration::from_millis(220)) {
            self.bell_flash = None;
        }

        self.page_drop = None;
        self.rename_drawn = false;
        if self.focus_mode && self.window.as_ref().is_some_and(|w| w.fullscreen().is_none()) && self.toast.is_none() {
            self.focus_mode = false;
        }
        let chrome = !self.focus_mode;
        if self.ws.sidebar && chrome {
            egui::SidePanel::left("sidebar")
                .resizable(true)
                .default_width(self.config.ui.sidebar_width)
                .width_range(150.0..=480.0)
                .show_separator_line(false)
                .frame(egui::Frame::new().fill(self.pal.panel))
                .show(ctx, |ui| self.sidebar(ui));
        }
        if chrome {
            egui::TopBottomPanel::top("topbar")
                .exact_height(TOPBAR_H)
                .show_separator_line(false)
                .frame(egui::Frame::new().fill(self.pal.panel).inner_margin(egui::Margin::symmetric(8, 0)))
                .show(ctx, |ui| self.topbar(ui));
        }
        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(self.pal.bg)).show(ctx, |ui| self.central(ui));

        self.find_bar(ctx);
        self.search_overlay(ctx);
        self.switcher_overlay(ctx);
        self.confirm_overlay(ctx);
        self.profile_prompt_overlay(ctx);
        self.history_overlay(ctx);
        self.server_overlay(ctx);
        self.shortcuts_overlay(ctx);
        self.toast_overlay(ctx);
        self.finish_drag(ctx);
        self.after_frame();
    }

    fn apply_style(&self, ctx: &egui::Context) {
        let p = &self.pal;
        let mut v = if p.dark { egui::Visuals::dark() } else { egui::Visuals::light() };
        // Flat: no shadows, one hairline border, popups clearly lifted from the background.
        let popup = mix(p.panel, p.fg, if p.dark { 0.07 } else { 0.02 });
        let line = mix(p.panel, p.fg, 0.14);
        v.panel_fill = p.panel;
        v.window_fill = popup;
        v.window_stroke = Stroke::new(1.0, line);
        v.window_shadow = egui::Shadow::NONE;
        v.popup_shadow = egui::Shadow::NONE;
        v.window_corner_radius = CornerRadius::same(12);
        v.menu_corner_radius = CornerRadius::same(10);
        v.extreme_bg_color = mix(popup, p.bg, 0.55);
        v.faint_bg_color = p.panel_hover;
        v.selection.bg_fill = p.accent.gamma_multiply(0.35);
        v.selection.stroke = Stroke::new(1.0, p.text);
        v.hyperlink_color = p.accent;
        v.override_text_color = Some(p.text);
        v.text_cursor.stroke = Stroke::new(1.5, p.accent);
        for w in [&mut v.widgets.noninteractive, &mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
            w.corner_radius = CornerRadius::same(7);
            w.fg_stroke = Stroke::new(1.0, p.text);
            w.bg_stroke = Stroke::NONE;
            w.expansion = 0.0;
        }
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, line);
        v.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
        v.widgets.inactive.bg_fill = mix(popup, p.fg, 0.06);
        v.widgets.hovered.weak_bg_fill = mix(popup, p.fg, 0.08);
        v.widgets.hovered.bg_fill = mix(popup, p.fg, 0.10);
        v.widgets.active.weak_bg_fill = p.panel_active;
        v.widgets.active.bg_fill = p.panel_active;
        v.widgets.open.weak_bg_fill = mix(popup, p.fg, 0.08);
        ctx.set_visuals(v);
        let size = self.config.ui.ui_font_size.max(13.0);
        let mono = self.mono_ui();
        let font = |size: f32| if mono { FontId::monospace(size) } else { FontId::proportional(size) };
        ctx.style_mut(|s| {
            use egui::TextStyle::*;
            s.text_styles.insert(Body, font(size));
            s.text_styles.insert(Button, font(size));
            s.text_styles.insert(Small, font(size - 2.0));
            s.text_styles.insert(Heading, font(size + 4.0));
            s.text_styles.insert(Monospace, FontId::monospace(size));
            s.spacing.item_spacing = vec2(8.0, 6.0);
            s.spacing.button_padding = vec2(10.0, 6.0);
            s.spacing.interact_size.y = 30.0;
            s.spacing.menu_margin = egui::Margin::same(6);
            s.spacing.window_margin = egui::Margin::same(16);
            s.spacing.menu_spacing = 4.0;
            s.interaction.tooltip_delay = 0.35;
            // Lists follow the wheel and trackpad directly instead of easing after them.
            s.scroll_animation = egui::style::ScrollAnimation::none();
        });
    }

    /// The look shared by every dialog.
    fn modal(&self, id: &str) -> egui::Modal {
        let p = &self.pal;
        let frame = egui::Frame::new()
            .fill(mix(p.panel, p.fg, if p.dark { 0.07 } else { 0.02 }))
            .stroke(Stroke::new(1.0, mix(p.panel, p.fg, 0.14)))
            .corner_radius(14)
            .inner_margin(egui::Margin::same(22));
        egui::Modal::new(Id::new(id)).frame(frame).backdrop_color(Color32::from_black_alpha(150))
    }

    fn after_frame(&mut self) {
        // A rename whose field is not on screen (sidebar hidden, page collapsed, filtered out,
        // focus mode) would otherwise swallow keys forever.
        if self.rename.is_some() && !self.rename_drawn {
            self.rename = None;
        }
        if self.window_focused {
            for sid in &self.visible {
                if let Some(c) = self.sessions.get_mut(sid).and_then(|s| s.claude.as_mut()) {
                    c.done = false;
                }
            }
        }
        if self.long_press.is_some() && !self.ctx.input(|i| i.pointer.any_down()) {
            self.long_press = None;
        }
        if let Some(set) = &mut self.select {
            set.retain(|s| self.sessions.contains_key(s));
        }
        let mut visible = if self.occluded { Vec::new() } else { self.visible.clone() };
        visible.sort_unstable();
        visible.dedup();
        if visible != self.subscribed {
            for sid in &visible {
                if let Some(sv) = self.sessions.get_mut(sid) {
                    sv.activity = false;
                    sv.bell = false;
                }
            }
            self.send(ClientMsg::Subscribe(visible.clone()));
            self.subscribed = visible;
        }
        if self.ws_dirty {
            self.send(ClientMsg::SetWorkspace(self.ws.clone()));
            self.ws_dirty = false;
        }
        // Focus in/out reporting for programs that ask for it (vim, Claude Code...).
        let want = self.ws.focused().map(|s| (s, self.window_focused));
        if want != self.reported_focus {
            if let Some((prev, true)) = self.reported_focus {
                if self.mode_of(prev) & mode::FOCUS_IN_OUT != 0 && want.is_none_or(|(s, f)| s != prev || !f) {
                    self.send(ClientMsg::Input(prev, b"\x1b[O".to_vec()));
                }
            }
            if let Some((cur, true)) = want {
                if self.mode_of(cur) & mode::FOCUS_IN_OUT != 0 && self.reported_focus != want {
                    self.send(ClientMsg::Input(cur, b"\x1b[I".to_vec()));
                }
            }
            self.reported_focus = want;
        }
        let mut title = self.ws.focused().map(|s| self.title_of(s)).unwrap_or_else(|| "sennya".into());
        if let Some(host) = self.target.host() {
            title = format!("{host} — {title}");
        }
        if title != self.last_title {
            if let Some(w) = &self.window {
                w.set_title(&title);
            }
            self.last_title = title;
        }
    }

    fn window_drag(&self, resp: &egui::Response) {
        if let Some(w) = &self.window {
            if resp.drag_started_by(egui::PointerButton::Primary) {
                let _ = w.drag_window();
            }
            if resp.double_clicked() {
                w.set_maximized(!w.is_maximized());
            }
        }
    }

    // ---------------------------------------------------------------- sidebar

    /// Labeled "Tabs | Grid" switch. Meant for a right-to-left layout.
    fn mode_switch(&mut self, ui: &mut Ui) {
        let grid = self.ws.mode == ViewMode::Grid;
        let tip = format!("Tabs: one terminal at a time. Grid: the whole page at once.  {}", self.keymap.label(Action::ToggleMode));
        let pal = self.pal.clone();
        egui::Frame::new().fill(mix(pal.panel, pal.fg, 0.06)).corner_radius(7).inner_margin(2).show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            if ui.selectable_label(grid, "Grid").on_hover_text(&tip).clicked() && !grid {
                self.do_action(Action::ToggleMode);
            }
            if ui.selectable_label(!grid, "Tabs").on_hover_text(&tip).clicked() && grid {
                self.do_action(Action::ToggleMode);
            }
        });
    }

    fn sidebar(&mut self, ui: &mut Ui) {
        let pal = self.pal.clone();
        let width = ui.available_width();
        let (hdr, hdr_resp) = ui.allocate_exact_size(vec2(width, TOPBAR_H), Sense::click_and_drag());
        self.window_drag(&hdr_resp);
        let mut hdr_ui = ui.new_child(
            UiBuilder::new().max_rect(hdr.shrink2(vec2(6.0, 5.0))).layout(Layout::right_to_left(Align::Center)),
        );
        if let Some(host) = self.target.host() {
            let online = self.conn.is_some() && !self.reconnecting && self.have_state;
            let pos = pos2(hdr.left() + self.traffic_lights_w().max(14.0), hdr.center().y);
            ui.painter().circle_filled(pos, 3.5, if online { pal.table[2] } else { pal.table[3] });
            ui.painter().text(pos + vec2(9.0, 0.0), Align2::LEFT_CENTER, host, self.font(13.0), pal.text);
        }
        let tip = format!("Hide sidebar  {}", self.keymap.label(Action::ToggleSidebar));
        if icons::button(&mut hdr_ui, Icon::Sidebar, &pal, tip).clicked() {
            self.do_action(Action::ToggleSidebar);
        }

        // The button and the filter below share height, corners, icon column and text column.
        const FIELD_H: f32 = 34.0;
        const ICON_X: f32 = 17.0;
        const TEXT_X: f32 = 34.0;
        let w = ui.available_width() - 16.0;
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            let (rect, resp) = ui.allocate_exact_size(vec2(w, FIELD_H), Sense::click());
            let fill = mix(pal.panel, pal.accent, if resp.hovered() { 0.28 } else { 0.2 });
            ui.painter().rect_filled(rect, 8.0, fill);
            icons::paint(ui.painter(), Icon::Plus, pos2(rect.left() + ICON_X, rect.center().y), pal.text);
            ui.painter().text(pos2(rect.left() + TEXT_X, rect.center().y), Align2::LEFT_CENTER, "New terminal", self.font(self.ui_size()), pal.text);
            ui.painter().text(
                pos2(rect.right() - 12.0, rect.center().y),
                Align2::RIGHT_CENTER,
                self.keymap.label(Action::NewTerminal),
                self.font(self.ui_size() - 1.0),
                pal.dim,
            );
            if resp.clicked() {
                self.do_action(Action::NewTerminal);
            }
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            let (field, _) = ui.allocate_exact_size(vec2(w, FIELD_H), Sense::hover());
            ui.painter().rect_filled(field, 8.0, mix(pal.panel, pal.fg, 0.06));
            icons::paint(ui.painter(), Icon::Search, pos2(field.left() + ICON_X, field.center().y), pal.dim);
            let clear = Rect::from_center_size(pos2(field.right() - 14.0, field.center().y), vec2(20.0, 20.0));
            // TextEdit adds its own 4 pt margin; start it that much earlier to line the text up.
            let edit_rect = Rect::from_min_max(pos2(field.left() + TEXT_X - 4.0, field.top()), pos2(clear.left(), field.bottom()));
            ui.put(
                edit_rect,
                egui::TextEdit::singleline(&mut self.filter)
                    .frame(false)
                    .hint_text("Filter terminals")
                    .vertical_align(Align::Center),
            );
            if !self.filter.is_empty() {
                let r = ui.interact(clear, Id::new("filter-clear"), Sense::click());
                icons::paint(ui.painter(), Icon::Close, clear.center(), if r.hovered() { pal.text } else { pal.dim });
                if r.clicked() {
                    self.filter.clear();
                }
            }
        });
        ui.add_space(6.0);

        egui::TopBottomPanel::bottom("sidebar-footer")
            .show_separator_line(false)
            .frame(egui::Frame::new().inner_margin(egui::Margin { left: 8, right: 8, top: 6, bottom: 10 }))
            .show_inside(ui, |ui| {
                if self.select.is_some() {
                    self.selection_bar(ui, &pal);
                } else {
                    ui.horizontal(|ui| {
                        let tip = format!("New page  {}", self.keymap.label(Action::NewPage));
                        if icon_btn(ui, "+  New page", 12.5, pal.dim, tip).clicked() {
                            self.do_action(Action::NewPage);
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let r = icons::button(ui, Icon::Gear, &pal, "Settings, themes, profiles, shortcuts");
                            egui::Popup::menu(&r).show(|ui| self.settings_menu(ui));
                        });
                    });
                }
            });

        let pages: Vec<(usize, u64, String, Vec<SessionId>)> =
            self.ws.pages.iter().enumerate().map(|(i, p)| (i, p.id, p.name.clone(), p.sessions())).collect();
        let filter = self.filter.trim().to_lowercase();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for (pi, page_id, name, sids) in pages {
                let shown: Vec<SessionId> = sids
                    .into_iter()
                    .filter(|s| {
                        filter.is_empty()
                            || self.title_of(*s).to_lowercase().contains(&filter)
                            || self.subtitle_of(*s).to_lowercase().contains(&filter)
                    })
                    .collect();
                if !filter.is_empty() && shown.is_empty() {
                    continue;
                }
                let collapsed = filter.is_empty() && self.collapsed.contains(&page_id);
                self.page_header(ui, pi, page_id, &name, &shown, collapsed, &pal);
                if !collapsed {
                    for sid in shown {
                        self.session_row(ui, sid, pi, &pal);
                    }
                }
                ui.add_space(4.0);
            }
        });
    }

    fn selection_bar(&mut self, ui: &mut Ui, pal: &Palette) {
        let selected: Vec<SessionId> = {
            let set = self.select.clone().unwrap_or_default();
            self.ws.all_sessions().into_iter().filter(|s| set.contains(s)).collect()
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{} selected", selected.len())).strong());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if icon_btn(ui, "Done", 12.5, pal.accent, "Leave selection (Esc)").clicked() {
                    self.select = None;
                }
                if icon_btn(ui, "All", 12.5, pal.dim, "Select every terminal").clicked() {
                    self.select = Some(self.ws.all_sessions().into_iter().collect());
                }
            });
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let enabled = !selected.is_empty();
            let close = egui::Button::new(RichText::new(format!("✕  Close {}", selected.len())).color(pal.danger))
                .fill(mix(pal.panel, pal.danger, 0.12))
                .corner_radius(6);
            if ui.add_enabled(enabled, close).clicked() {
                self.request_close(selected.clone());
                self.select = None;
            }
            let pages: Vec<(usize, String)> = self.ws.pages.iter().enumerate().map(|(i, p)| (i, p.name.clone())).collect();
            ui.add_enabled_ui(enabled, |ui| {
                ui.menu_button("Move to ▾", |ui| {
                    for (i, name) in &pages {
                        if ui.button(name).clicked() {
                            let page_id = self.ws.pages[*i].id;
                            for sid in &selected {
                                if let Some(idx) = self.ws.pages.iter().position(|p| p.id == page_id) {
                                    self.ws.move_to_page(*sid, idx);
                                }
                            }
                            self.ws_dirty = true;
                            self.select = None;
                            ui.close();
                        }
                    }
                    if ui.button("New page").clicked() {
                        let idx = self.ws.new_page();
                        let page_id = self.ws.pages[idx].id;
                        for sid in &selected {
                            if let Some(idx) = self.ws.pages.iter().position(|p| p.id == page_id) {
                                self.ws.move_to_page(*sid, idx);
                            }
                        }
                        self.ws_dirty = true;
                        self.select = None;
                        ui.close();
                    }
                });
            });
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn page_header(&mut self, ui: &mut Ui, pi: usize, page_id: u64, name: &str, sids: &[SessionId], collapsed: bool, pal: &Palette) {
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), (self.ui_size() * 2.1).round()), Sense::click());
        let active = pi == self.ws.active_page;
        let body = rect.shrink2(vec2(6.0, 2.0));
        let painter = ui.painter();
        if self.drag_pane.is_some() && resp.contains_pointer() {
            self.page_drop = Some(pi);
            painter.rect_filled(body, 6.0, pal.accent.gamma_multiply(0.25));
        } else if resp.hovered() {
            painter.rect_filled(body, 6.0, pal.panel_hover);
        }
        let color = if active { pal.text } else { pal.dim };
        icons::paint(painter, Icon::Chevron(!collapsed), pos2(body.left() + 11.0, body.center().y), if resp.hovered() { pal.text } else { pal.dim });
        let name_rect = Rect::from_min_max(pos2(body.left() + 18.0, body.top() + 1.0), pos2(body.right() - 30.0, body.bottom() - 1.0));
        if self.inline_rename(ui, name_rect, &RenameTarget::Page(page_id), RenamePlace::Sidebar) {
            return;
        }
        let painter = ui.painter();
        let label = painter.text(
            pos2(body.left() + 22.0, body.center().y),
            Align2::LEFT_CENTER,
            name.to_uppercase(),
            self.font(self.ui_size() - 2.0),
            color,
        );
        painter.text(
            pos2(label.right() + 7.0, body.center().y),
            Align2::LEFT_CENTER,
            sids.len().to_string(),
            self.font(self.ui_size() - 2.5),
            pal.dim,
        );
        let unseen = sids.iter().any(|s| self.sessions.get(s).is_some_and(|v| v.activity || v.bell));
        let right = Rect::from_center_size(pos2(body.right() - 12.0, body.center().y), vec2(20.0, 20.0));
        let selecting = self.select.is_some();
        let all_selected = selecting && !sids.is_empty() && sids.iter().all(|s| self.select.as_ref().is_some_and(|set| set.contains(s)));
        if selecting {
            checkbox(painter, right.center(), all_selected, pal);
        } else if resp.hovered() {
            let over = resp.hover_pos().is_some_and(|p| right.contains(p));
            if over {
                painter.rect_filled(right, 5.0, pal.panel_active);
            }
            icons::paint(painter, Icon::Plus, right.center(), if over { pal.text } else { pal.dim });
        } else if let Some((state, done)) = collapsed.then(|| self.page_claude_status(sids)).flatten() {
            status_badge(ui, painter, right.center(), state, done, pal);
        } else if collapsed && unseen {
            painter.circle_filled(right.center(), 3.5, pal.accent);
        }
        if resp.hovered() && !selecting {
            resp.clone().on_hover_text_at_pointer(if collapsed { "Click to expand" } else { "Click to collapse · + adds a terminal here" });
        }
        if resp.clicked() {
            let on_right = resp.interact_pointer_pos().is_some_and(|p| right.contains(p));
            if on_right && selecting {
                let set = self.select.get_or_insert_with(HashSet::new);
                for s in sids {
                    if all_selected {
                        set.remove(s);
                    } else {
                        set.insert(*s);
                    }
                }
            } else if on_right {
                self.new_terminal(Placement::Auto { page: page_id });
                self.collapsed.remove(&page_id);
            } else if !self.collapsed.remove(&page_id) {
                self.collapsed.insert(page_id);
            }
        }
        if resp.double_clicked() {
            self.start_rename(Rename { place: RenamePlace::Sidebar, target: RenameTarget::Page(page_id), text: name.to_string(), focus: true });
        }
        let sids = sids.to_vec();
        resp.context_menu(|ui| {
            if ui.button("Switch to this page").clicked() {
                self.ws.active_page = pi;
                self.ws_dirty = true;
                ui.close();
            }
            if ui.button("New terminal here").clicked() {
                self.new_terminal(Placement::Auto { page: page_id });
                ui.close();
            }
            if ui.button(if collapsed { "Expand" } else { "Collapse" }).clicked() {
                if !self.collapsed.remove(&page_id) {
                    self.collapsed.insert(page_id);
                }
                ui.close();
            }
            if ui.button("Select its terminals").clicked() {
                self.select.get_or_insert_with(HashSet::new).extend(sids.iter().copied());
                ui.close();
            }
            if ui.button("Rename page").clicked() {
                self.start_rename(Rename { place: RenamePlace::Sidebar, target: RenameTarget::Page(page_id), text: name.to_string(), focus: true });
                ui.close();
            }
            ui.separator();
            if ui.button(RichText::new("Close page").color(self.pal.danger)).clicked() {
                self.request_close(sids.clone());
                ui.close();
            }
        });
    }

    fn session_row(&mut self, ui: &mut Ui, sid: SessionId, pi: usize, pal: &Palette) {
        let (title_size, sub_size) = (self.ui_size(), self.ui_size() - 2.0);
        let row_h = (title_size * 1.35 + sub_size * 1.35 + 14.0).round();
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click_and_drag());
        let active = self.ws.focused() == Some(sid) && pi == self.ws.active_page;
        let selecting = self.select.is_some();
        let checked = self.select.as_ref().is_some_and(|s| s.contains(&sid));
        let body = rect.shrink2(vec2(6.0, 1.0));

        // Press and hold to start selecting.
        let mut hold = 0.0;
        if !selecting && self.long_press.is_none() && resp.is_pointer_button_down_on() {
            let (held, moved) = ui.input(|i| {
                let held = i.pointer.press_start_time().map(|t| i.time - t).unwrap_or(0.0);
                let moved = i.pointer.press_origin().zip(i.pointer.interact_pos()).map(|(a, b)| a.distance(b)).unwrap_or(0.0);
                (held, moved)
            });
            if moved < 6.0 {
                if held >= LONG_PRESS {
                    self.select = Some(HashSet::from([sid]));
                    self.long_press = Some(sid);
                } else {
                    hold = (held / LONG_PRESS) as f32;
                    ui.ctx().request_repaint_after(Duration::from_millis(16));
                }
            }
        }
        let swallowed = self.long_press == Some(sid);
        let selecting = self.select.is_some();
        let checked = checked || swallowed;

        let painter = ui.painter();
        if checked {
            painter.rect_filled(body, 6.0, pal.panel_active);
        } else if active && !selecting {
            painter.rect_filled(body, 6.0, pal.panel_active);
        } else if resp.hovered() {
            painter.rect_filled(body, 6.0, pal.panel_hover);
        }
        if hold > 0.15 {
            let w = body.width() * hold.min(1.0);
            painter.rect_filled(Rect::from_min_size(body.min, vec2(w, body.height())), 6.0, pal.accent.gamma_multiply(0.18));
        }
        let mut text_x = body.left() + 14.0;
        if selecting {
            checkbox(painter, pos2(body.left() + 16.0, body.center().y), checked, pal);
            text_x += 20.0;
        }
        let (activity, bell, _restored) = self
            .sessions
            .get(&sid)
            .map(|s| (s.activity, s.bell, s.info.restored))
            .unwrap_or_default();
        let close = Rect::from_center_size(pos2(body.right() - 14.0, body.center().y), vec2(20.0, 20.0));
        let text_w = body.right() - 34.0 - text_x;
        let title = self.title_of(sid);
        let sub = self.subtitle_of(sid);
        let hidden = self.ws.pages.get(pi).is_some_and(|p| p.hidden.contains(&sid));
        let title_color = if hidden {
            pal.dim
        } else if active || checked {
            pal.text
        } else {
            mix(pal.text, pal.dim, 0.25)
        };
        // While renaming, the field takes the whole row (name and folder line).
        let title_rect = Rect::from_min_max(pos2(text_x - 6.0, body.top() + 5.0), pos2(body.right() - 8.0, body.bottom() - 5.0));
        let renaming = self.inline_rename(ui, title_rect, &RenameTarget::Session(sid), RenamePlace::Sidebar);
        let painter = ui.painter();
        if !renaming {
            let g = truncated(painter, &title, self.font(title_size), title_color, text_w);
            painter.galley(pos2(text_x, body.top() + 6.0), g, pal.text);
        }
        let status = self.claude_status(sid);
        // Claude's state is shown by the badge on the right, not in words.
        let sub_color = pal.dim;
        if !renaming {
            let g = truncated(painter, &sub, self.font(sub_size), sub_color, text_w);
            painter.galley(pos2(text_x, body.top() + 7.0 + title_size * 1.35), g, sub_color);
        }
        if resp.hovered() && !selecting {
            let over = resp.hover_pos().is_some_and(|p| close.contains(p));
            if over {
                painter.rect_filled(close, 5.0, mix(pal.panel, pal.danger, 0.2));
            }
            icons::paint(painter, Icon::Close, close.center(), if over { pal.danger } else { pal.dim });
        } else if let Some((state, done)) = status.filter(|(st, done)| *st != 0 || *done) {
            status_badge(ui, painter, close.center(), state, done, pal);
        } else if bell {
            icons::paint(painter, Icon::Bell, close.center(), pal.accent);
        } else if activity {
            painter.circle_filled(close.center(), 3.5, pal.accent);
        } else if hidden {
            icons::paint(painter, Icon::Minimize, close.center(), pal.dim);
        }
        if hidden && resp.hovered() && !selecting {
            resp.clone().on_hover_text("Hidden from the grid, still running. Click to show it");
        }

        if swallowed {
            return;
        }
        if !selecting && resp.drag_started() {
            self.drag_pane = Some(sid);
        }
        let cmd = if cfg!(target_os = "macos") { self.modifiers.super_key() } else { self.modifiers.control_key() };
        if resp.clicked() {
            if selecting || cmd {
                let set = self.select.get_or_insert_with(HashSet::new);
                if !set.remove(&sid) {
                    set.insert(sid);
                }
                if set.is_empty() {
                    self.select = None;
                }
            } else if resp.interact_pointer_pos().is_some_and(|p| close.contains(p)) {
                self.request_close(vec![sid]);
            } else {
                self.focus_session(sid);
            }
        }
        if resp.double_clicked() && !selecting {
            self.do_action(Action::Rename);
        }
        let pages: Vec<(usize, String)> = self.ws.pages.iter().enumerate().map(|(i, p)| (i, p.name.clone())).collect();
        resp.context_menu(|ui| {
            if ui.button("Select…").on_hover_text("Tip: press and hold a terminal, or ⌘-click").clicked() {
                self.select.get_or_insert_with(HashSet::new).insert(sid);
                ui.close();
            }
            if ui.button("Rename").clicked() {
                self.focus_session(sid);
                self.do_action(Action::Rename);
                ui.close();
            }
            if hidden {
                if ui.button("Show in the grid").clicked() {
                    self.ws.unhide(sid);
                    self.ws_dirty = true;
                    ui.close();
                }
            } else if menu_item(ui, "Hide, keep running", self.keymap.label(Action::HidePane), &self.pal).clicked() {
                self.ws.hide(sid);
                self.ws_dirty = true;
                ui.close();
            }
            let claude = self.sessions.get(&sid).and_then(|s| s.claude.clone()).filter(|c| !c.id.is_empty());
            if let Some(ClaudeView { id, title, .. }) = claude {
                let cwd = self.sessions.get(&sid).map(|s| s.info.cwd.clone()).unwrap_or_default();
                if ui.button("Copy Claude resume command").on_hover_text(title).clicked() {
                    let cmd = crate::template::cd_then(&cwd, &format!("claude --resume {id}"));
                    if let Some(cb) = self.clipboard() {
                        let _ = cb.set_text(cmd);
                    }
                    ui.close();
                }
            }
            ui.menu_button("Move to page", |ui| {
                for (i, name) in &pages {
                    if *i != pi && ui.button(name).clicked() {
                        self.ws.move_to_page(sid, *i);
                        self.ws_dirty = true;
                        ui.close();
                    }
                }
                if ui.button("New page").clicked() {
                    let idx = self.ws.new_page();
                    self.ws.move_to_page(sid, idx);
                    self.ws_dirty = true;
                    ui.close();
                }
            });
            ui.separator();
            if ui.button(RichText::new("Close").color(self.pal.danger)).clicked() {
                self.request_close(vec![sid]);
                ui.close();
            }
        });
    }

    fn settings_menu(&mut self, ui: &mut Ui) {
        ui.set_min_width(220.0);
        let current = self.config.theme.name.clone();
        ui.menu_button("Theme", |ui| {
            for name in THEME_NAMES {
                if ui.radio(current == name, name).clicked() {
                    match crate::config::set_config_value("theme", "name", Some(name)) {
                        Ok(()) => self.reload_config(),
                        Err(err) => self.toast(format!("Could not write config: {err}")),
                    }
                    ui.close();
                }
            }
        });
        if ui.button("Keyboard shortcuts…").clicked() {
            self.shortcuts = Some(ShortcutsView { filter: String::new(), capture: None });
            ui.close();
        }
        ui.menu_button("Profiles", |ui| {
            let profiles: Vec<(usize, String, String)> = self
                .config
                .profiles
                .iter()
                .enumerate()
                .map(|(i, p)| (i, p.name.clone(), p.description.clone()))
                .collect();
            if profiles.is_empty() {
                ui.label(RichText::new("No profiles yet.\nAdd [[profile]] entries to the config.").color(self.pal.dim));
            }
            for (i, name, description) in profiles {
                if menu_item(ui, &name, self.keymap.label(Action::Profile(i as u16)), &self.pal).on_hover_text(description).clicked() {
                    self.start_profile(i);
                    ui.close();
                }
            }
            ui.separator();
            if ui.button("Add or edit profiles…").clicked() {
                self.do_action(Action::OpenConfig);
                ui.close();
            }
        });
        ui.menu_button("Servers", |ui| {
            let remotes = self.config.remotes.clone();
            for r in &remotes {
                let label = if r.name.is_empty() { r.host.clone() } else { r.name.clone() };
                if ui.button(format!("Open {label}")).on_hover_text(format!("A window attached to the sennya daemon on {}", r.host)).clicked() {
                    let _ = open_window(Some(&r.host));
                    ui.close();
                }
            }
            if remotes.is_empty() {
                ui.label(RichText::new("Add [[remote]] servers to the config,\nafter `sennya remote install HOST`.").color(self.pal.dim));
            }
            ui.separator();
            if ui.button("Connect to a server…").clicked() {
                self.server_prompt = Some((String::new(), true));
                ui.close();
            }
        });
        if menu_item(ui, "New window", self.keymap.label(Action::NewWindow), &self.pal).clicked() {
            self.do_action(Action::NewWindow);
            ui.close();
        }
        if menu_item(ui, "Focus mode (fullscreen)", self.keymap.label(Action::FocusMode), &self.pal).clicked() {
            self.do_action(Action::FocusMode);
            ui.close();
        }
        if menu_item(ui, "Claude chats…", self.keymap.label(Action::ClaudeHistory), &self.pal).clicked() {
            self.open_history();
            ui.close();
        }
        if menu_item(ui, "Edit config…", self.keymap.label(Action::OpenConfig), &self.pal).clicked() {
            self.do_action(Action::OpenConfig);
            ui.close();
        }
        if ui.button("Reload config").clicked() {
            self.reload_config();
            ui.close();
        }
        ui.separator();
        if menu_item(ui, "Quit, keep terminals running", self.keymap.label(Action::Quit), &self.pal).clicked() {
            self.quit = true;
            ui.close();
        }
        if ui.button("Restart session daemon…").on_hover_text("Applies an update to the part that keeps terminals running").clicked() {
            self.confirm = Some(Confirm::RestartDaemon);
            ui.close();
        }
        if ui.button(RichText::new("Kill all terminals and quit…").color(self.pal.danger)).clicked() {
            self.confirm = Some(Confirm::KillAll);
            ui.close();
        }
    }

    // ---------------------------------------------------------------- top bar

    fn topbar(&mut self, ui: &mut Ui) {
        let pal = self.pal.clone();
        let rect = ui.max_rect();
        let bg = ui.interact(rect, Id::new("topbar-drag"), Sense::click_and_drag());
        self.window_drag(&bg);
        ui.horizontal_centered(|ui| {
            if !self.ws.sidebar {
                ui.add_space((self.traffic_lights_w() - 8.0).max(0.0));
                let tip = format!("Show sidebar  {}", self.keymap.label(Action::ToggleSidebar));
                if icons::button(ui, Icon::Sidebar, &pal, tip).clicked() {
                    self.do_action(Action::ToggleSidebar);
                }
                ui.add_space(4.0);
            }
            match self.ws.mode {
                ViewMode::Grid => self.page_tabs(ui, &pal),
                ViewMode::Tabs => {
                    if let Some(sid) = self.ws.focused() {
                        let title = self.title_of(sid);
                        let sub = self.subtitle_of(sid);
                        ui.label(RichText::new(title).strong());
                        ui.label(RichText::new(sub).color(pal.dim).size(11.0));
                    }
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let tip = format!("Search all terminals  {}", self.keymap.label(Action::GlobalSearch));
                if icons::button(ui, Icon::Search, &pal, tip).clicked() {
                    self.do_action(Action::GlobalSearch);
                }
                let tip = format!("Claude Code chats: resume any conversation  {}", self.keymap.label(Action::ClaudeHistory));
                if icons::button(ui, Icon::History, &pal, tip).clicked() {
                    self.open_history();
                }
                if !self.ws.sidebar {
                    let r = icons::button(ui, Icon::Gear, &pal, "Settings, themes, profiles, shortcuts");
                    egui::Popup::menu(&r).show(|ui| self.settings_menu(ui));
                }
                ui.add_space(4.0);
                self.mode_switch(ui);
                ui.add_space(4.0);
                if self.ws.mode == ViewMode::Grid {
                    self.layout_menu(ui, &pal);
                }
                let tip = format!("Split down: new terminal below this one  {}", self.keymap.label(Action::SplitDown));
                if icons::button(ui, Icon::SplitDown, &pal, tip).clicked() {
                    self.do_action(Action::SplitDown);
                }
                let tip = format!("Split right: new terminal next to this one  {}", self.keymap.label(Action::SplitRight));
                if icons::button(ui, Icon::SplitRight, &pal, tip).clicked() {
                    self.do_action(Action::SplitRight);
                }
            });
        });
    }

    fn page_tabs(&mut self, ui: &mut Ui, pal: &Palette) {
        let pages: Vec<(usize, u64, String, Vec<SessionId>)> =
            self.ws.pages.iter().enumerate().map(|(i, p)| (i, p.id, p.name.clone(), p.sessions())).collect();
        let many = pages.len() > 1;
        for (i, page_id, name, sids) in pages {
            let active = i == self.ws.active_page;
            // Even padding on both sides; the count sits right after the name and turns into
            // the close button on hover, so no space is reserved for it.
            let font = self.font(self.ui_size());
            let count = sids.len().to_string();
            let (name_w, count_w) = ui.fonts_mut(|f| {
                (
                    f.layout_no_wrap(name.clone(), font.clone(), pal.text).size().x,
                    f.layout_no_wrap(count.clone(), font.clone(), pal.text).size().x,
                )
            });
            const PAD: f32 = 12.0;
            const GAP: f32 = 8.0;
            let slot_w = count_w.max(14.0);
            let renaming = matches!(&self.rename, Some(Rename { place: RenamePlace::Tabs, target: RenameTarget::Page(p), .. }) if *p == page_id);
            let natural = PAD + name_w + GAP + slot_w + PAD;
            let width = if renaming { natural.max(160.0) } else { natural };
            let (rect, resp) = ui.allocate_exact_size(vec2(width, 28.0), Sense::click());
            if self.inline_rename(ui, rect, &RenameTarget::Page(page_id), RenamePlace::Tabs) {
                continue;
            }
            let painter = ui.painter();
            let hovered = resp.hovered();
            if self.drag_pane.is_some() && resp.contains_pointer() {
                self.page_drop = Some(i);
                painter.rect_stroke(rect, 7.0, Stroke::new(1.5, pal.accent), StrokeKind::Inside);
            }
            if active {
                painter.rect_filled(rect, 7.0, pal.panel_active);
            } else if hovered {
                painter.rect_filled(rect, 7.0, pal.panel_hover);
            }
            let name_color = if active { pal.text } else { pal.dim };
            painter.text(pos2(rect.left() + PAD, rect.center().y), Align2::LEFT_CENTER, &name, font.clone(), name_color);
            let slot_center = pos2(rect.right() - PAD - slot_w / 2.0, rect.center().y);
            let close = Rect::from_center_size(slot_center, vec2(18.0, 18.0));
            let over_close = many && hovered && resp.hover_pos().is_some_and(|p| close.contains(p));
            if many && hovered {
                if over_close {
                    painter.rect_filled(close, 4.0, mix(pal.panel, pal.danger, 0.25));
                }
                icons::paint(painter, Icon::Close, close.center(), if over_close { pal.danger } else { pal.dim });
            } else {
                painter.text(slot_center, Align2::CENTER_CENTER, &count, font, mix(name_color, pal.panel, 0.35));
            }
            if hovered {
                resp.clone().on_hover_text(format!("{}\nDouble-click to rename · drop a terminal here to move it", self.keymap.label(Action::GotoPage((i + 1).min(9) as u8))));
            }
            if resp.clicked() {
                if resp.interact_pointer_pos().is_some_and(|p| many && close.contains(p)) {
                    self.request_close(sids.clone());
                } else {
                    self.ws.active_page = i;
                    self.ws_dirty = true;
                }
            }
            if resp.double_clicked() {
                self.start_rename(Rename { place: RenamePlace::Tabs, target: RenameTarget::Page(page_id), text: name.clone(), focus: true });
            }
            resp.context_menu(|ui| {
                if ui.button("Rename page").clicked() {
                    self.start_rename(Rename { place: RenamePlace::Tabs, target: RenameTarget::Page(page_id), text: name.clone(), focus: true });
                    ui.close();
                }
                if ui.button(RichText::new("Close page").color(self.pal.danger)).clicked() {
                    self.request_close(sids.clone());
                    ui.close();
                }
            });
        }
        let tip = format!("New page  {}", self.keymap.label(Action::NewPage));
        if icons::button(ui, Icon::Plus, pal, tip).clicked() {
            self.do_action(Action::NewPage);
        }
    }

    /// Segmented control with one icon per arrangement. Meant for a right-to-left layout.
    fn layout_menu(&mut self, ui: &mut Ui, pal: &Palette) {
        let page = self.ws.active_page;
        let current = self.ws.page().arrange;
        let options = [
            (Arrange::MainStack, Icon::LayoutMainStack, "Main + stack: one big terminal, the rest stacked on the right"),
            (Arrange::Rows, Icon::LayoutRows, "Rows: terminals stacked top to bottom"),
            (Arrange::Columns, Icon::LayoutColumns, "Columns: terminals side by side"),
            (Arrange::Grid, Icon::LayoutGrid, "Grid: an even grid that re-tiles as terminals come and go"),
        ];
        let rebalance = self.keymap.label(Action::Rebalance);
        egui::Frame::new().fill(mix(pal.panel, pal.fg, 0.06)).corner_radius(7).inner_margin(2).show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for (arrange, icon, tip) in options {
                let active = current == arrange;
                let (rect, resp) = ui.allocate_exact_size(vec2(28.0, 22.0), Sense::click());
                if active {
                    ui.painter().rect_filled(rect, 5.0, pal.panel_active);
                } else if resp.hovered() {
                    ui.painter().rect_filled(rect, 5.0, pal.panel_hover);
                }
                let color = if active { pal.accent } else if resp.hovered() { pal.text } else { pal.dim };
                icons::paint(ui.painter(), icon, rect.center(), color);
                let tip = if current == Arrange::Manual {
                    format!("{tip}\n\nThis page is arranged by hand; click to re-tile it")
                } else {
                    format!("{tip}\nClick again to rebalance ({rebalance})")
                };
                if resp.on_hover_text(tip).clicked() {
                    self.ws.set_arrange(page, arrange);
                    self.ws_dirty = true;
                }
            }
        });
    }

    // ---------------------------------------------------------------- panes

    fn central(&mut self, ui: &mut Ui) {
        let rect = ui.max_rect();
        self.central_rect = rect;
        self.pane_rects.clear();
        self.visible.clear();
        self.drop_target = None;
        self.autoscroll = false;
        let pal = self.pal.clone();
        if let Some(err) = self.conn_error.clone() {
            ui.scope_builder(UiBuilder::new().max_rect(rect.shrink(40.0)), |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(rect.height() / 3.0);
                    ui.label(RichText::new(err).color(pal.danger));
                    if ui.button("Retry").clicked() {
                        self.reconnect();
                    }
                });
            });
            return;
        }
        if self.daemon_outdated.is_some() && self.conn.is_none() {
            let mut restart = false;
            ui.scope_builder(UiBuilder::new().max_rect(rect.shrink(40.0)), |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(rect.height() / 3.0);
                    ui.label(RichText::new("sennya was updated").size(18.0).strong());
                    ui.add_space(6.0);
                    ui.label(RichText::new("The running session daemon is an older version. Restart it to continue.").color(pal.dim));
                    ui.label(RichText::new("Terminals come back in the same folders with their history; programs running in them stop.").color(pal.dim));
                    ui.add_space(12.0);
                    if ui.button(RichText::new("   Restart session daemon   ").color(pal.accent)).clicked() {
                        restart = true;
                    }
                });
            });
            if restart {
                self.restart_daemon();
            }
            return;
        }
        if !self.have_state {
            return;
        }
        if self.daemon_outdated.is_some() {
            let bar = Rect::from_min_size(rect.min, vec2(rect.width(), 34.0));
            let mut restart = false;
            ui.scope_builder(UiBuilder::new().max_rect(bar), |ui| {
                ui.painter().rect_filled(bar, 0.0, mix(pal.panel, pal.accent, 0.18));
                ui.horizontal_centered(|ui| {
                    ui.add_space(12.0);
                    ui.label("sennya was updated. Restart the session daemon to enable profiles, Claude chats and the CLI.");
                    ui.label(RichText::new("Terminals come back in their folders with their history; running programs stop.").color(pal.dim).size(11.0));
                    if ui.button("Restart now").clicked() {
                        restart = true;
                    }
                });
            });
            if restart {
                self.restart_daemon();
            }
        }
        if self.ws.all_sessions().is_empty() {
            ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(rect.height() / 2.0 - 40.0);
                    ui.label(RichText::new("No terminals").size(16.0).color(pal.dim));
                    ui.add_space(8.0);
                    if menu_item(ui, "New terminal", self.keymap.label(Action::NewTerminal), &self.pal).clicked() {
                        self.do_action(Action::NewTerminal);
                    }
                });
            });
            return;
        }
        match self.ws.mode {
            ViewMode::Tabs => {
                if let Some(sid) = self.ws.focused() {
                    self.pane(ui, sid, rect, false);
                }
            }
            ViewMode::Grid => {
                let page = self.ws.page().clone();
                let mut area = rect;
                if !page.hidden.is_empty() {
                    let dock = Rect::from_min_max(pos2(rect.left(), rect.bottom() - DOCK_H), rect.max);
                    area.max.y -= DOCK_H;
                    self.hidden_dock(ui, dock, &page.hidden);
                }
                if let Some(z) = page.zoomed.filter(|z| page.contains(*z)) {
                    self.pane(ui, z, area, true);
                } else if let Some(root) = &page.root {
                    let mut panes = Vec::new();
                    let mut divs = Vec::new();
                    layout_node(root, area, self.config.ui.pane_gap.max(1.0), &mut panes, &mut divs);
                    let single = panes.len() == 1;
                    for (sid, r) in panes {
                        self.pane(ui, sid, r, !single);
                    }
                    self.dividers(ui, &divs);
                }
            }
        }
        if let Some(host) = self.target.host().filter(|_| self.conn.is_none() || self.reconnecting) {
            let text = format!("Reconnecting to {host}…  terminals keep running on the server");
            let pill = Rect::from_center_size(pos2(rect.center().x, rect.top() + 24.0), vec2(text.len() as f32 * 7.0 + 40.0, 30.0));
            ui.painter().rect_filled(pill, 15.0, mix(pal.panel, pal.table[3], 0.25));
            ui.painter().text(pill.center(), Align2::CENTER_CENTER, text, self.font(13.0), pal.text);
            ui.ctx().request_repaint_after(Duration::from_millis(500));
        }
        if self.hovering_files {
            ui.painter().rect_filled(rect.shrink(8.0), 10.0, pal.accent.gamma_multiply(0.12));
            ui.painter().rect_stroke(rect.shrink(8.0), 10.0, Stroke::new(2.0, pal.accent), StrokeKind::Inside);
            ui.painter().text(rect.center(), Align2::CENTER_CENTER, "Drop to paste the path", self.font(16.0), pal.text);
        }
    }

    fn pane(&mut self, ui: &mut Ui, sid: SessionId, rect: Rect, titled: bool) {
        let pal = self.pal.clone();
        let Some(m) = self.metrics.clone() else { return };
        let multi = titled && self.ws.mode == ViewMode::Grid;
        let focused = self.ws.focused() == Some(sid);
        let show_title = multi && self.config.ui.pane_titles;
        let painter = ui.painter_at(rect.expand(1.0));
        let radius = 0.0;
        let mut content = rect;

        if show_title {
            let title_rect = Rect::from_min_size(rect.min, vec2(rect.width(), PANE_TITLE_H));
            content.min.y += PANE_TITLE_H;
            let t = ui.interact(title_rect, Id::new(("pane-title", sid)), Sense::click_and_drag());
            let fill = if focused { mix(pal.panel, pal.accent, 0.10) } else { pal.panel };
            painter.rect_filled(title_rect, 0.0, fill);
            let close = Rect::from_center_size(pos2(title_rect.right() - 12.0, title_rect.center().y), vec2(16.0, 16.0));
            let zoom = close.translate(vec2(-20.0, 0.0));
            let title = self.title_of(sid);
            let g = truncated(&painter, &title, self.font(self.ui_size() - 1.0), if focused { pal.text } else { pal.dim }, title_rect.width() - 60.0);
            painter.galley(pos2(title_rect.left() + 10.0, title_rect.center().y - g.size().y / 2.0), g, pal.text);
            if t.hovered() {
                let hp = t.hover_pos();
                let col = |r: Rect, c: Color32| if hp.is_some_and(|p| r.contains(p)) { c } else { pal.dim };
                icons::paint(&painter, Icon::Zoom, zoom.center(), col(zoom, pal.text));
                icons::paint(&painter, Icon::Close, close.center(), col(close, pal.danger));
            }
            if t.drag_started() {
                self.drag_pane = Some(sid);
            }
            if t.clicked() {
                let p = t.interact_pointer_pos();
                if p.is_some_and(|p| close.contains(p)) {
                    self.request_close(vec![sid]);
                } else if p.is_some_and(|p| zoom.contains(p)) {
                    self.focus_session(sid);
                    self.do_action(Action::ZoomPane);
                } else {
                    self.focus_session(sid);
                }
            }
            if t.double_clicked() {
                self.focus_session(sid);
                self.do_action(Action::ZoomPane);
            }
            t.context_menu(|ui| self.pane_menu(ui, sid));
        }

        let pad = self.config.ui.padding.max(2.0);
        let inner = content.shrink2(vec2(pad, (pad * 0.6).max(2.0)));
        let (cols, rows) = m.grid_size(inner.size());
        let (cw, ch) = m.cell_px();
        let resize = self.sessions.get_mut(&sid).is_some_and(|sv| {
            let size = (cols, rows, cw, ch);
            let changed = sv.sent_size != size;
            sv.sent_size = size;
            changed
        });
        if resize {
            self.send(ClientMsg::Resize { id: sid, cols, rows, cell_w: cw, cell_h: ch });
        }

        let resp = ui.interact(content, Id::new(("pane", sid)), Sense::click_and_drag());
        let origin = snap(inner.min, m.ppp);
        self.pane_mouse(ui, sid, origin, inner, &resp, &m);
        if !show_title {
            resp.context_menu(|ui| self.pane_menu(ui, sid));
        }

        let cursor_on = !self.config.terminal.cursor_blink
            || !self.window_focused
            || (self.last_input.elapsed().as_millis() / BLINK.as_millis()) % 2 == 0;
        let link: Vec<(u16, u16, u16)> = match &self.hover_link {
            Some((s, _, ranges)) if *s == sid => ranges.clone(),
            _ => Vec::new(),
        };
        if let Some(frame) = self.sessions.get(&sid).and_then(|s| s.frame.as_ref()) {
            let tp = ui.painter_at(content);
            let opts = DrawOpts { focused: focused && self.window_focused, cursor_on, link: &link };
            term_view::draw(&tp, ui.ctx(), &mut self.glyphs, origin, frame, &m, &pal, &opts);
            if frame.display_offset > 0 {
                let label = format!("↑ {} / {}", frame.display_offset, frame.history);
                let pos = pos2(content.right() - 10.0, content.top() + 8.0);
                let r = tp.text(pos, Align2::RIGHT_TOP, label, self.font(self.ui_size() - 2.5), pal.dim);
                tp.rect_filled(r.expand(4.0), 4.0, pal.panel.gamma_multiply(0.85));
                tp.text(pos, Align2::RIGHT_TOP, format!("↑ {} / {}", frame.display_offset, frame.history), self.font(self.ui_size() - 2.5), pal.text);
            }
        }

        if multi && focused {
            painter.rect_filled(Rect::from_min_size(rect.min, vec2(rect.width(), 2.0)), 0.0, pal.accent);
        }
        if multi && !show_title && self.drag_pane.is_none() {
            self.pane_chip(ui, &painter, sid, rect, &pal);
        }
        if let Some((b, at)) = self.bell_flash {
            if b == sid && at.elapsed() < Duration::from_millis(200) {
                painter.rect_filled(rect, radius, pal.accent.gamma_multiply(0.10));
            }
        }
        if let Some(src) = self.drag_pane {
            if src != sid {
                if let Some(p) = ui.ctx().pointer_hover_pos().filter(|p| rect.contains(*p)) {
                    let edge = edge_for(rect, p);
                    self.drop_target = Some((sid, edge));
                    let r = edge_rect(rect, edge).shrink(3.0);
                    painter.rect_filled(r, 6.0, pal.accent.gamma_multiply(0.22));
                    painter.rect_stroke(r, 6.0, Stroke::new(1.5, pal.accent), StrokeKind::Inside);
                }
            }
        }
        self.visible.push(sid);
        self.pane_rects.push((sid, rect));
    }

    /// Small toolbar shown only while the pointer is near the top of a pane:
    /// move (drag), hide, zoom, close.
    fn pane_chip(&mut self, ui: &mut Ui, painter: &egui::Painter, sid: SessionId, rect: Rect, pal: &Palette) {
        let Some(p) = ui.ctx().pointer_hover_pos().filter(|p| rect.contains(*p)) else { return };
        let button = vec2(26.0, 24.0);
        let pad = 3.0;
        let buttons = [
            (Icon::Grip, "Drag to move"),
            (Icon::Minimize, "Hide, keeps running"),
            (Icon::Zoom, "Zoom"),
            (Icon::Close, "Close"),
        ];
        let size = vec2(button.x * buttons.len() as f32 + pad * 2.0, button.y + pad * 2.0);
        let chip = Rect::from_min_size(pos2(rect.right() - size.x - 8.0, rect.top() + 8.0), size);
        if p.y > chip.bottom() + 8.0 && !chip.contains(p) {
            return;
        }
        painter.rect_filled(chip, 8.0, pal.panel);
        painter.rect_stroke(chip, 8.0, Stroke::new(1.0, mix(pal.panel, pal.fg, 0.14)), StrokeKind::Inside);
        for (i, (icon, tip)) in buttons.into_iter().enumerate() {
            let r = Rect::from_min_size(chip.min + vec2(pad + i as f32 * button.x, pad), button);
            let sense = if icon == Icon::Grip { Sense::click_and_drag() } else { Sense::click() };
            let resp = ui.interact(r, Id::new(("pane-chip", sid, i)), sense);
            let hot = if icon == Icon::Close { pal.danger } else { pal.text };
            if resp.hovered() {
                painter.rect_filled(r, 6.0, pal.panel_hover);
            }
            icons::paint(painter, icon, r.center(), if resp.hovered() { hot } else { pal.dim });
            if icon == Icon::Grip && resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::Grab);
            }
            if resp.drag_started() {
                self.drag_pane = Some(sid);
            }
            let clicked = resp.clicked();
            let double = resp.double_clicked();
            resp.on_hover_text(tip);
            if clicked {
                self.focus_session(sid);
                match icon {
                    Icon::Minimize => self.do_action(Action::HidePane),
                    Icon::Zoom => self.do_action(Action::ZoomPane),
                    Icon::Close => self.request_close(vec![sid]),
                    _ => {}
                }
            }
            if double && icon == Icon::Grip {
                self.do_action(Action::ZoomPane);
            }
        }
    }

    /// Strip under the grid with the page's hidden terminals; click one to bring it back.
    fn hidden_dock(&mut self, ui: &mut Ui, rect: Rect, hidden: &[SessionId]) {
        let pal = self.pal.clone();
        ui.painter().rect_filled(rect, 0.0, pal.panel);
        ui.painter().hline(rect.x_range(), rect.top(), Stroke::new(1.0, pal.border));
        let label = ui.painter().text(
            pos2(rect.left() + 12.0, rect.center().y),
            Align2::LEFT_CENTER,
            "Hidden",
            self.font(self.ui_size() - 2.0),
            pal.dim,
        );
        let mut x = label.right() + 10.0;
        for &sid in hidden {
            let title = self.title_of(sid);
            let g = truncated(ui.painter(), &title, self.font(self.ui_size() - 1.0), pal.text, 180.0);
            let (activity, bell) = self.sessions.get(&sid).map(|s| (s.activity, s.bell)).unwrap_or_default();
            let w = g.size().x + if activity || bell || self.claude_status(sid).is_some() { 30.0 } else { 20.0 };
            if x + w > rect.right() - 8.0 {
                break;
            }
            let chip = Rect::from_min_size(pos2(x, rect.top() + 5.0), vec2(w, rect.height() - 10.0));
            let resp = ui.interact(chip, Id::new(("dock", sid)), Sense::click_and_drag());
            ui.painter().rect_filled(chip, 5.0, if resp.hovered() { pal.panel_active } else { pal.panel_hover });
            ui.painter().galley(pos2(chip.left() + 10.0, chip.center().y - g.size().y / 2.0), g, pal.text);
            if let Some((state, done)) = self.claude_status(sid).filter(|(st, d)| *st != 0 || *d) {
                status_badge(ui, ui.painter(), pos2(chip.right() - 11.0, chip.center().y), state, done, &pal);
            } else if bell || activity {
                ui.painter().circle_filled(pos2(chip.right() - 11.0, chip.center().y), 3.5, pal.accent);
            }
            if resp.clicked() {
                self.focus_session(sid);
            }
            if resp.drag_started() {
                self.drag_pane = Some(sid);
            }
            resp.on_hover_text("Still running. Click to show it in the grid, or drag it onto a pane");
            x = chip.right() + 6.0;
        }
    }

    fn pane_menu(&mut self, ui: &mut Ui, sid: SessionId) {
        self.focus_session(sid);
        for (label, action) in [
            ("Copy", Action::Copy),
            ("Paste", Action::Paste),
            ("Select all", Action::SelectAll),
            ("Clear scrollback", Action::ClearScrollback),
        ] {
            if menu_item(ui, label, self.keymap.label(action), &self.pal).clicked() {
                self.do_action(action);
                ui.close();
            }
        }
        ui.separator();
        for (label, action) in [("Split right", Action::SplitRight), ("Split down", Action::SplitDown), ("Zoom", Action::ZoomPane), ("Rename", Action::Rename)] {
            if menu_item(ui, label, self.keymap.label(action), &self.pal).clicked() {
                self.do_action(action);
                ui.close();
            }
        }
        let page = self.ws.page();
        if let Some(zone) = page.root.as_ref().and_then(|r| r.parent_split(sid)) {
            let is_zone = page.zone == Some(zone);
            let label = if is_zone { "✓ New terminals open in this area" } else { "New terminals open in this area" };
            if ui.button(label).on_hover_text("Turns the page into a manual layout where ⌘T adds terminals to this pane's group").clicked() {
                let p = self.ws.page_mut();
                p.zone = if is_zone { None } else { Some(zone) };
                p.arrange = Arrange::Manual;
                self.ws_dirty = true;
                ui.close();
            }
        }
        if menu_item(ui, "Hide, keep running", self.keymap.label(Action::HidePane), &self.pal).clicked() {
            self.do_action(Action::HidePane);
            ui.close();
        }
        ui.separator();
        if menu_item(ui, "Close", self.keymap.label(Action::CloseTerminal), &self.pal).clicked() {
            self.request_close(vec![sid]);
            ui.close();
        }
    }

    fn dividers(&mut self, ui: &mut Ui, divs: &[Div]) {
        for d in divs {
            let resp = ui.interact(d.rect, Id::new(("div", d.split, d.index)), Sense::drag());
            let active = resp.hovered() || resp.dragged();
            if active {
                ui.ctx().set_cursor_icon(match d.dir {
                    Dir::Horizontal => CursorIcon::ResizeHorizontal,
                    Dir::Vertical => CursorIcon::ResizeVertical,
                });
            }
            let c = d.rect.center();
            let (a, b) = match d.dir {
                Dir::Horizontal => (pos2(c.x, d.rect.top()), pos2(c.x, d.rect.bottom())),
                Dir::Vertical => (pos2(d.rect.left(), c.y), pos2(d.rect.right(), c.y)),
            };
            let stroke = if active { Stroke::new(2.0, self.pal.accent) } else { Stroke::new(1.0, self.pal.border) };
            ui.painter().line_segment([a, b], stroke);
            if resp.dragged() {
                let delta = match d.dir {
                    Dir::Horizontal => resp.drag_delta().x,
                    Dir::Vertical => resp.drag_delta().y,
                } / d.span;
                if delta != 0.0 {
                    let min = (120.0 / d.span).min(0.45);
                    let page = self.ws.page_mut();
                    if let Some(root) = page.root.as_mut() {
                        root.resize_split(d.split, d.index, delta, min);
                    }
                    page.arrange = Arrange::Manual;
                    self.ws_dirty = true;
                }
            }
        }
    }

    fn finish_drag(&mut self, ctx: &egui::Context) {
        let Some(src) = self.drag_pane else { return };
        if let Some(p) = ctx.pointer_hover_pos() {
            let title = self.title_of(src);
            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, Id::new("drag-label")));
            let r = painter.text(p + vec2(14.0, 10.0), Align2::LEFT_TOP, &title, self.font(self.ui_size() - 1.0), self.pal.text);
            painter.rect_filled(r.expand(6.0), 6.0, self.pal.panel_active);
            painter.text(p + vec2(14.0, 10.0), Align2::LEFT_TOP, &title, self.font(self.ui_size() - 1.0), self.pal.text);
        }
        if ctx.input(|i| i.pointer.any_released() || !i.pointer.any_down()) {
            if let Some(page) = self.page_drop {
                self.ws.move_to_page(src, page);
                self.ws_dirty = true;
            } else if let Some((target, edge)) = self.drop_target {
                self.ws.move_session(src, target, edge);
                self.ws_dirty = true;
            }
            self.drag_pane = None;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn pane_mouse(&mut self, ui: &Ui, sid: SessionId, origin: Pos2, inner: Rect, resp: &egui::Response, m: &Metrics) {
        let Some((cols, rows, term_mode)) =
            self.sessions.get(&sid).and_then(|s| s.frame.as_ref()).map(|f| (f.cols, f.rows, f.mode))
        else {
            if resp.clicked() {
                self.focus_session(sid);
            }
            return;
        };
        let mods = self.modifiers;
        let cell_of = |p: Pos2| -> (u16, u16, bool) {
            let x = ((p.x - origin.x) / m.cell_w).max(0.0);
            let y = ((p.y - origin.y) / m.cell_h).max(0.0);
            let col = (x.floor() as u16).min(cols.saturating_sub(1));
            let row = (y.floor() as u16).min(rows.saturating_sub(1));
            (col, row, x.fract() > 0.5)
        };
        if resp.clicked() || resp.drag_started() || resp.secondary_clicked() || resp.middle_clicked() {
            self.focus_session(sid);
        }
        let cmd = if cfg!(target_os = "macos") { mods.super_key() } else { mods.control_key() };
        let mouse_mode = term_mode & mode::MOUSE_MODE != 0 && !mods.shift_key();
        let hover = resp.hover_pos().filter(|p| inner.contains(*p));
        let mut out: Vec<ClientMsg> = Vec::new();

        if self.hover_link.as_ref().is_some_and(|(s, _, _)| *s == sid) {
            self.hover_link = None;
        }
        if let Some(p) = hover {
            if cmd {
                let (c, r, _) = cell_of(p);
                if let Some((url, ranges)) =
                    self.sessions.get(&sid).and_then(|s| s.frame.as_ref()).and_then(|f| term_view::link_at(f, r, c))
                {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    self.hover_link = Some((sid, url, ranges));
                }
            } else if !mouse_mode && self.drag_pane.is_none() {
                ui.ctx().set_cursor_icon(CursorIcon::Text);
            }
        }
        if resp.clicked() && cmd {
            if let Some((_, url, _)) = self.hover_link.as_ref().filter(|(s, _, _)| *s == sid) {
                if !term_view::open_url(url) {
                    self.toast("Blocked link with an unknown scheme");
                }
                return;
            }
        }

        if mouse_mode {
            let events = ui.input(|i| i.events.clone());
            let on_top = resp.hovered() || resp.is_pointer_button_down_on();
            let Some(sv) = self.sessions.get_mut(&sid) else { return };
            for ev in events {
                match ev {
                    egui::Event::PointerButton { pos, button, pressed, .. }
                        if (pressed && on_top && !cmd && inner.contains(pos)) || (!pressed && sv.mouse_down.is_some()) =>
                    {
                        let b = match button {
                            egui::PointerButton::Primary => 0,
                            egui::PointerButton::Middle => 1,
                            egui::PointerButton::Secondary => 2,
                            _ => continue,
                        };
                        let (c, r, _) = cell_of(pos);
                        if let Some(bytes) = keys::mouse_report(b, pressed, false, c, r, mods, term_mode) {
                            out.push(ClientMsg::Input(sid, bytes));
                        }
                        sv.mouse_down = pressed.then_some(b);
                        sv.last_cell = Some((c, r));
                    }
                    egui::Event::PointerMoved(pos) if inner.contains(pos) => {
                        let motion = term_mode & mode::MOUSE_MOTION != 0;
                        let drag = term_mode & mode::MOUSE_DRAG != 0 && sv.mouse_down.is_some();
                        let (c, r, _) = cell_of(pos);
                        if (motion || drag) && sv.last_cell != Some((c, r)) {
                            sv.last_cell = Some((c, r));
                            let b = sv.mouse_down.unwrap_or(3);
                            if let Some(bytes) = keys::mouse_report(b, true, true, c, r, mods, term_mode) {
                                out.push(ClientMsg::Input(sid, bytes));
                            }
                        }
                    }
                    _ => {}
                }
            }
        } else {
            let primary = egui::PointerButton::Primary;
            if resp.triple_clicked() {
                if let Some(p) = resp.interact_pointer_pos() {
                    let (c, r, right) = cell_of(p);
                    out.push(ClientMsg::Select(sid, SelectCmd::Start { row: r, col: c, right, kind: SelectKind::Line }));
                }
            } else if resp.double_clicked() {
                if let Some(p) = resp.interact_pointer_pos() {
                    let (c, r, right) = cell_of(p);
                    out.push(ClientMsg::Select(sid, SelectCmd::Start { row: r, col: c, right, kind: SelectKind::Word }));
                }
            } else if resp.drag_started_by(primary) {
                if let Some(p) = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos()) {
                    let (c, r, right) = cell_of(p);
                    let kind = if mods.alt_key() { SelectKind::Block } else { SelectKind::Simple };
                    out.push(ClientMsg::Select(sid, SelectCmd::Start { row: r, col: c, right, kind }));
                    if let Some(sv) = self.sessions.get_mut(&sid) {
                        sv.last_cell = Some((c, r));
                    }
                }
            } else if resp.dragged_by(primary) {
                if let Some(p) = resp.interact_pointer_pos() {
                    if p.y < inner.top() {
                        out.push(ClientMsg::Scroll(sid, ScrollCmd::Lines(1)));
                        self.autoscroll = true;
                    } else if p.y > inner.bottom() {
                        out.push(ClientMsg::Scroll(sid, ScrollCmd::Lines(-1)));
                        self.autoscroll = true;
                    }
                    let (c, r, right) = cell_of(p);
                    if let Some(sv) = self.sessions.get_mut(&sid) {
                        if sv.last_cell != Some((c, r)) || self.autoscroll {
                            sv.last_cell = Some((c, r));
                            out.push(ClientMsg::Select(sid, SelectCmd::Update { row: r, col: c, right }));
                        }
                    }
                }
            } else if resp.drag_stopped_by(primary) {
                if self.config.terminal.copy_on_select {
                    let req = self.req();
                    self.copy_reqs.insert(req);
                    out.push(ClientMsg::Copy { req, id: sid });
                }
            } else if resp.clicked() && !cmd {
                out.push(ClientMsg::Select(sid, SelectCmd::Clear));
            }
            if resp.middle_clicked() {
                self.paste();
            }
        }

        if hover.is_some() {
            // egui turns Shift+wheel into horizontal scrolling; treat it as vertical here.
            let dy = ui.input(|i| if i.raw_scroll_delta.y != 0.0 { i.raw_scroll_delta.y } else { i.raw_scroll_delta.x });
            if dy != 0.0 {
                let mult = self.config.terminal.scroll_multiplier / 3.0;
                if let Some(sv) = self.sessions.get_mut(&sid) {
                    sv.scroll_acc += dy * mult;
                    let lines = (sv.scroll_acc / m.cell_h).trunc() as i32;
                    if lines != 0 {
                        sv.scroll_acc -= lines as f32 * m.cell_h;
                        let (c, r, _) = cell_of(hover.unwrap());
                        if mouse_mode {
                            let b = if lines > 0 { 64 } else { 65 };
                            for _ in 0..lines.unsigned_abs().min(20) {
                                if let Some(bytes) = keys::mouse_report(b, true, false, c, r, mods, term_mode) {
                                    out.push(ClientMsg::Input(sid, bytes));
                                }
                            }
                        } else if term_mode & mode::ALT_SCREEN != 0 && term_mode & mode::ALTERNATE_SCROLL != 0 {
                            let app = term_mode & mode::APP_CURSOR != 0;
                            let seq: &[u8] = match (lines > 0, app) {
                                (true, true) => b"\x1bOA",
                                (true, false) => b"\x1b[A",
                                (false, true) => b"\x1bOB",
                                (false, false) => b"\x1b[B",
                            };
                            out.push(ClientMsg::Input(sid, seq.repeat(lines.unsigned_abs().min(30) as usize)));
                        } else {
                            out.push(ClientMsg::Scroll(sid, ScrollCmd::Lines(lines)));
                        }
                    }
                }
            }
        }
        for msg in out {
            self.send(msg);
        }
    }

    // ---------------------------------------------------------------- overlays

    fn find_bar(&mut self, ctx: &egui::Context) {
        let Some(mut f) = self.find.take() else { return };
        let rect = self.pane_rects.iter().find(|(s, _)| *s == f.session).map(|(_, r)| *r).unwrap_or(self.central_rect);
        let mut open = true;
        let mut search: Option<bool> = None;
        let pal = self.pal.clone();
        egui::Area::new(Id::new("find-bar"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos2(rect.right() - 380.0, rect.top() + 28.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let color = f.not_found.then_some(pal.danger);
                        let r = text_field(ui, &mut f.query, "Find", 190.0, color, &pal);
                        if f.focus {
                            r.request_focus();
                            f.focus = false;
                        }
                        if r.changed() {
                            search = Some(true);
                        }
                        if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                            search = Some(!ui.input(|i| i.modifiers.shift));
                            r.request_focus();
                        }
                        if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Escape)) {
                            open = false;
                        }
                        if ui.selectable_label(f.case, "Aa").on_hover_text("Match case").clicked() {
                            f.case = !f.case;
                            search = Some(true);
                        }
                        if ui.selectable_label(f.regex, ".*").on_hover_text("Regular expression").clicked() {
                            f.regex = !f.regex;
                            search = Some(true);
                        }
                        if ui.button("↑").on_hover_text("Older (Enter)").clicked() {
                            search = Some(true);
                        }
                        if ui.button("↓").on_hover_text("Newer (Shift+Enter)").clicked() {
                            search = Some(false);
                        }
                        if ui.button("✕").clicked() {
                            open = false;
                        }
                    });
                });
            });
        if let Some(backwards) = search {
            if !f.query.is_empty() {
                let query = SearchQuery { text: f.query.clone(), regex: f.regex, case_sensitive: f.case };
                self.send(ClientMsg::Find { id: f.session, query, backwards });
            }
        }
        if open {
            self.find = Some(f);
        }
    }

    fn search_overlay(&mut self, ctx: &egui::Context) {
        let Some(mut s) = self.search.take() else { return };
        let mut open = true;
        let mut reveal: Option<SearchHit> = None;
        let pal = self.pal.clone();
        let titles: HashMap<SessionId, String> = s
            .hits
            .iter()
            .map(|h| h.session)
            .collect::<HashSet<_>>()
            .into_iter()
            .map(|sid| {
                let page = self.ws.page_of(sid).and_then(|p| self.ws.pages.get(p)).map(|p| p.name.clone()).unwrap_or_default();
                (sid, format!("{page} › {}", self.title_of(sid)))
            })
            .collect();
        let width = 760.0_f32.min(self.central_rect.width() - 40.0);
        let list_h = list_height(self.central_rect.height() + TOPBAR_H);
        let modal = self.modal("global-search").show(ctx, |ui| {
            ui.set_width(width);
            ui.horizontal(|ui| {
                let r = search_field(ui, &mut s.query, "Search every terminal, scrollback included", &pal, width - 96.0);
                if s.focus {
                    r.request_focus();
                    s.focus = false;
                }
                if r.changed() {
                    s.edited_at = Some(Instant::now());
                }
                if ui.selectable_label(s.case, "Aa").clicked() {
                    s.case = !s.case;
                    s.edited_at = Some(Instant::now());
                }
                if ui.selectable_label(s.regex, ".*").clicked() {
                    s.regex = !s.regex;
                    s.edited_at = Some(Instant::now());
                }
            });
            ui.add_space(6.0);
            let (down, up, enter, esc) = ui.input(|i| {
                (i.key_pressed(Key::ArrowDown), i.key_pressed(Key::ArrowUp), i.key_pressed(Key::Enter), i.key_pressed(Key::Escape))
            });
            if esc {
                open = false;
            }
            if !s.hits.is_empty() {
                if down {
                    s.selected = (s.selected + 1).min(s.hits.len() - 1);
                }
                if up {
                    s.selected = s.selected.saturating_sub(1);
                }
                if enter {
                    reveal = s.hits.get(s.selected).cloned();
                }
            }
            if s.searching {
                ui.label(RichText::new("Searching…").color(pal.dim));
            } else if !s.query.is_empty() && s.hits.is_empty() && s.edited_at.is_none() {
                ui.label(RichText::new("No matches").color(pal.dim));
            }
            egui::ScrollArea::vertical().min_scrolled_height(list_h).max_height(list_h).auto_shrink([false, false]).show(ui, |ui| {
                let mut last = None;
                for (i, hit) in s.hits.iter().enumerate() {
                    if last != Some(hit.session) {
                        last = Some(hit.session);
                        ui.add_space(4.0);
                        ui.label(RichText::new(titles.get(&hit.session).cloned().unwrap_or_default()).size(11.0).color(pal.accent));
                    }
                    let mut job = egui::text::LayoutJob::default();
                    let chars: Vec<char> = hit.snippet.chars().collect();
                    let a = (hit.start_col as usize).min(chars.len());
                    let b = if hit.end_line == hit.line { (hit.end_col as usize + 1).min(chars.len()) } else { chars.len() };
                    let font = FontId::monospace(11.5);
                    let fmt = |c: Color32, bg: Color32| egui::TextFormat { font_id: font.clone(), color: c, background: bg, ..Default::default() };
                    let left: String = chars[..a].iter().collect();
                    let mid: String = chars[a..b.max(a)].iter().collect();
                    let right: String = chars[b.max(a)..].iter().collect();
                    let left = if left.chars().count() > 40 { format!("…{}", left.chars().rev().take(40).collect::<Vec<_>>().into_iter().rev().collect::<String>()) } else { left };
                    job.append(left.trim_start(), 0.0, fmt(pal.dim, Color32::TRANSPARENT));
                    job.append(&mid, 0.0, fmt(pal.text, pal.accent.gamma_multiply(0.35)));
                    job.append(&right, 0.0, fmt(pal.dim, Color32::TRANSPARENT));
                    job.wrap = egui::text::TextWrapping::truncate_at_width(ui.available_width() - 10.0);
                    let r = ui.selectable_label(i == s.selected, job);
                    if i == s.selected && (down || up) {
                        r.scroll_to_me(None);
                    }
                    if r.clicked() {
                        reveal = Some(hit.clone());
                    }
                }
            });
        });
        if modal.should_close() {
            open = false;
        }
        if s.edited_at.is_some_and(|t| t.elapsed() > Duration::from_millis(160)) {
            s.edited_at = None;
            if s.query.trim().is_empty() {
                s.hits.clear();
            } else {
                s.req = self.req();
                s.searching = true;
                let query = SearchQuery { text: s.query.clone(), regex: s.regex, case_sensitive: s.case };
                self.send(ClientMsg::Search { req: s.req, query });
            }
        }
        if let Some(hit) = reveal {
            self.focus_session(hit.session);
            self.send(ClientMsg::Reveal(hit));
            open = false;
        }
        if open {
            self.search = Some(s);
        }
    }

    fn switcher_overlay(&mut self, ctx: &egui::Context) {
        let Some(mut s) = self.switcher.take() else { return };
        let mut open = true;
        let mut pick: Option<SessionId> = None;
        let pal = self.pal.clone();
        let entries: Vec<(SessionId, String, String)> = self
            .ws
            .pages
            .iter()
            .flat_map(|p| p.sessions().into_iter().map(move |sid| (sid, p.name.clone())))
            .map(|(sid, page)| (sid, self.title_of(sid), format!("{page} › {}", self.subtitle_of(sid))))
            .filter(|(_, t, sub)| fuzzy(&format!("{t} {sub}"), &s.query))
            .collect();
        let profiles: Vec<(usize, String)> = self
            .config
            .profiles
            .iter()
            .enumerate()
            .filter(|(_, p)| fuzzy(&format!("{} {}", p.name, p.description), &s.query))
            .map(|(i, p)| (i, format!("▶  {}    {}", p.name, self.keymap.label(Action::Profile(i as u16)))))
            .collect();
        let mut run_profile: Option<usize> = None;
        let total = profiles.len() + entries.len();
        let list_h = list_height(self.central_rect.height() + TOPBAR_H).min(460.0);
        let modal = self.modal("switcher").show(ctx, |ui| {
            ui.set_width(600.0);
            let r = search_field(ui, &mut s.query, "Jump to a terminal or run a profile…", &pal, 600.0);
            if s.focus {
                r.request_focus();
                s.focus = false;
            }
            if r.changed() {
                s.selected = 0;
            }
            let (down, up, enter, esc) = ui.input(|i| {
                (i.key_pressed(Key::ArrowDown), i.key_pressed(Key::ArrowUp), i.key_pressed(Key::Enter), i.key_pressed(Key::Escape))
            });
            if esc {
                open = false;
            }
            if total > 0 {
                if down {
                    s.selected = (s.selected + 1).min(total - 1);
                }
                if up {
                    s.selected = s.selected.saturating_sub(1);
                }
                if enter {
                    if s.selected < profiles.len() {
                        run_profile = Some(profiles[s.selected].0);
                    } else {
                        pick = entries.get(s.selected - profiles.len()).map(|e| e.0);
                    }
                }
            }
            ui.add_space(4.0);
            egui::ScrollArea::vertical().min_scrolled_height(list_h).max_height(list_h).auto_shrink([false, false]).show(ui, |ui| {
                for (i, (index, label)) in profiles.iter().enumerate() {
                    let r = ui.selectable_label(i == s.selected, RichText::new(label).color(pal.accent));
                    if i == s.selected && (down || up) {
                        r.scroll_to_me(None);
                    }
                    if r.clicked() {
                        run_profile = Some(*index);
                    }
                }
                for (i, (sid, title, sub)) in entries.iter().enumerate() {
                    let i = i + profiles.len();
                    let r = ui.selectable_label(i == s.selected, format!("{title}   —   {sub}"));
                    if i == s.selected && (down || up) {
                        r.scroll_to_me(None);
                    }
                    if r.clicked() {
                        pick = Some(*sid);
                    }
                }
                if total == 0 {
                    ui.label(RichText::new("Nothing matches").color(pal.dim));
                }
            });
        });
        if modal.should_close() {
            open = false;
        }
        if let Some(sid) = pick {
            self.focus_session(sid);
            open = false;
        }
        if let Some(i) = run_profile {
            open = false;
            self.start_profile(i);
        }
        if open {
            self.switcher = Some(s);
        }
    }

    fn confirm_overlay(&mut self, ctx: &egui::Context) {
        let Some(c) = self.confirm.take() else { return };
        let mut decision: Option<bool> = None;
        let pal = self.pal.clone();
        let (title, body, ok) = match &c {
            Confirm::Paste { text, .. } => (
                "Paste multiple lines?".to_string(),
                format!("The shell will run each line immediately.\n\n{}", text.lines().take(6).collect::<Vec<_>>().join("\n")),
                "Paste",
            ),
            Confirm::Close { what, .. } => ("Close?".to_string(), what.clone(), "Close"),
            Confirm::KillAll => (
                "Kill every terminal?".to_string(),
                "All shells and programs stop and saved sessions are deleted.".to_string(),
                "Kill all",
            ),
            Confirm::RestartDaemon => (
                "Restart the session daemon?".to_string(),
                "Applies an update of sennya. Terminals come back in their folders with their history; programs running in them stop. Claude chats reopen when you look at their terminals.".to_string(),
                "Restart",
            ),
        };
        let ok_color = if matches!(c, Confirm::Paste { .. }) { pal.accent } else { pal.danger };
        let modal = self.modal("confirm").show(ctx, |ui| {
            ui.set_width(420.0);
            ui.label(RichText::new(title).size(17.0).strong());
            ui.add_space(6.0);
            ui.label(RichText::new(body).size(14.0).color(pal.dim));
            ui.add_space(20.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let ok_button = egui::Button::new(RichText::new(ok).strong().color(Color32::WHITE))
                    .fill(ok_color)
                    .corner_radius(8)
                    .min_size(vec2(96.0, 34.0));
                if ui.add(ok_button).clicked() || ui.input(|i| i.key_pressed(Key::Enter)) {
                    decision = Some(true);
                }
                let cancel = egui::Button::new("Cancel").fill(mix(pal.panel, pal.fg, 0.12)).corner_radius(8).min_size(vec2(96.0, 34.0));
                if ui.add(cancel).clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                    decision = Some(false);
                }
            });
        });
        if modal.should_close() && decision.is_none() {
            decision = Some(false);
        }
        match decision {
            None => self.confirm = Some(c),
            Some(false) => {}
            Some(true) => match c {
                Confirm::Paste { session, text } => {
                    let m = self.mode_of(session);
                    self.send(ClientMsg::Input(session, keys::paste_bytes(&text, m)));
                }
                Confirm::Close { sessions, .. } => self.close_sessions(&sessions),
                Confirm::KillAll => {
                    self.send(ClientMsg::Shutdown);
                    self.quit = true;
                }
                Confirm::RestartDaemon => {
                    self.daemon_outdated = self.daemon_outdated.or(self.daemon_pid);
                    self.restart_daemon();
                }
            },
        }
    }

    fn profile_prompt_overlay(&mut self, ctx: &egui::Context) {
        let Some(mut p) = self.profile_prompt.take() else { return };
        let Some(profile) = self.config.profiles.get(p.index).cloned() else { return };
        let mut open = true;
        let mut run = false;
        let modal = self.modal("profile-prompt").show(ctx, |ui| {
            ui.set_width(400.0);
            ui.label(RichText::new(profile.name.clone()).strong().size(17.0));
            if !profile.description.is_empty() {
                ui.label(RichText::new(&profile.description).color(self.pal.dim));
            }
            ui.add_space(6.0);
            for (i, label) in p.labels.iter().enumerate() {
                ui.label(label);
                let r = text_field(ui, &mut p.values[i], "", 400.0, None, &self.pal);
                if p.focus && i == 0 {
                    r.request_focus();
                }
                if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) && i + 1 == p.labels.len() {
                    run = true;
                }
            }
            p.focus = false;
            ui.add_space(16.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let go = egui::Button::new(RichText::new("Run").strong().color(Color32::WHITE)).fill(self.pal.accent).corner_radius(8).min_size(vec2(96.0, 34.0));
                if ui.add(go).clicked() {
                    run = true;
                }
                let cancel = egui::Button::new("Cancel").fill(mix(self.pal.panel, self.pal.fg, 0.12)).corner_radius(8).min_size(vec2(96.0, 34.0));
                if ui.add(cancel).clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                    open = false;
                }
            });
        });
        if modal.should_close() {
            open = false;
        }
        if run {
            let vars = p.labels.iter().zip(&p.values).map(|(l, v)| (format!("prompt:{l}"), v.trim().to_string())).collect();
            self.send_profile(p.index, vars);
            open = false;
        }
        if open {
            self.profile_prompt = Some(p);
        }
    }

    fn history_overlay(&mut self, ctx: &egui::Context) {
        let Some(mut h) = self.history.take() else { return };
        let mut open = true;
        let mut resume: Option<crate::claude::ClaudeSession> = None;
        let pal = self.pal.clone();
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let ago = |t: u64| {
            let d = now.saturating_sub(t);
            match d {
                0..=59 => "just now".to_string(),
                60..=3599 => format!("{} min ago", d / 60),
                3600..=86_399 => format!("{} h ago", d / 3600),
                _ => format!("{} d ago", d / 86_400),
            }
        };
        let shown: Vec<crate::claude::ClaudeSession> = h
            .sessions
            .iter()
            .filter(|c| fuzzy(&format!("{} {} {} {}", c.title, c.cwd, c.branch, c.last_prompt), &h.query))
            .cloned()
            .collect();
        let width = 760.0_f32.min(self.central_rect.width() - 40.0);
        let list_h = list_height(self.central_rect.height() + TOPBAR_H);
        let modal = self.modal("claude-history").show(ctx, |ui| {
            ui.set_width(width);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Claude chats").strong().size(17.0));
                ui.label(RichText::new("click one to resume it in a new terminal").color(pal.dim));
            });
            ui.add_space(8.0);
            let r = search_field(ui, &mut h.query, "Filter by title, folder, branch or prompt", &pal, width);
            if h.focus {
                r.request_focus();
                h.focus = false;
            }
            if r.changed() {
                h.selected = 0;
                h.keyboard = false;
            }
            let (down, up, enter, esc) = ui.input(|i| {
                (i.key_pressed(Key::ArrowDown), i.key_pressed(Key::ArrowUp), i.key_pressed(Key::Enter), i.key_pressed(Key::Escape))
            });
            if esc {
                open = false;
            }
            if !shown.is_empty() {
                if down {
                    h.selected = if h.keyboard { (h.selected + 1).min(shown.len() - 1) } else { 0 };
                    h.keyboard = true;
                }
                if up {
                    h.selected = h.selected.saturating_sub(1);
                    h.keyboard = true;
                }
                if enter {
                    resume = shown.get(h.selected).cloned();
                }
            }
            ui.add_space(4.0);
            if h.loading {
                ui.label(RichText::new("Loading…").color(pal.dim));
            } else if shown.is_empty() {
                ui.label(RichText::new("No Claude Code chats found").color(pal.dim));
            }
            ui.add_space(6.0);
            egui::ScrollArea::vertical().min_scrolled_height(list_h).max_height(list_h).auto_shrink([false, false]).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                for (i, c) in shown.iter().enumerate() {
                    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 56.0), Sense::click());
                    // The keyboard cursor only shows once the arrows are used.
                    let selected = h.keyboard && i == h.selected;
                    if selected || resp.hovered() {
                        ui.painter().rect_filled(rect, 8.0, if selected { pal.panel_active } else { mix(pal.panel, pal.fg, 0.10) });
                    }
                    let w = rect.width() - 28.0;
                    let g = truncated(ui.painter(), &c.title, self.font(15.0), pal.text, w);
                    ui.painter().galley(pos2(rect.left() + 14.0, rect.top() + 8.0), g, pal.text);
                    let mut meta = vec![paths::tilde(&c.cwd)];
                    if !c.branch.is_empty() {
                        meta.push(c.branch.clone());
                    }
                    meta.push(ago(c.modified));
                    if !c.terminal.is_empty() {
                        meta.push(format!("in {}", c.terminal));
                    }
                    let g = truncated(ui.painter(), &meta.join("  ·  "), self.font(self.ui_size()), pal.dim, w);
                    ui.painter().galley(pos2(rect.left() + 14.0, rect.top() + 31.0), g, pal.dim);
                    if selected && (down || up) {
                        resp.scroll_to_me(None);
                    }
                    let tip = if c.last_prompt.is_empty() { c.first_prompt.clone() } else { format!("Last prompt: {}", c.last_prompt) };
                    let resp = resp.on_hover_text(tip);
                    if resp.clicked() {
                        resume = Some(c.clone());
                    }
                    resp.context_menu(|ui| {
                        if ui.button("Resume in a new terminal").clicked() {
                            resume = Some(c.clone());
                            ui.close();
                        }
                        if ui.button("Copy resume command").clicked() {
                            let cmd = crate::template::cd_then(&c.cwd, &format!("claude --resume {}", c.id));
                            if let Ok(mut cb) = arboard::Clipboard::new() {
                                let _ = cb.set_text(cmd);
                            }
                            ui.close();
                        }
                    });
                }
            });
        });
        if modal.should_close() {
            open = false;
        }
        if let Some(c) = resume {
            self.resume_claude(&c);
            open = false;
        }
        if open {
            self.history = Some(h);
        }
    }

    fn server_overlay(&mut self, ctx: &egui::Context) {
        let Some((mut host, mut focus)) = self.server_prompt.take() else { return };
        let mut open = true;
        let mut go = false;
        let pal = self.pal.clone();
        let modal = self.modal("server").show(ctx, |ui| {
            ui.set_width(420.0);
            ui.label(RichText::new("Connect to a server").size(17.0).strong());
            ui.add_space(4.0);
            ui.label(RichText::new("Opens a window attached to the sennya daemon on that machine. Its terminals keep running when the network drops.").color(pal.dim));
            ui.add_space(10.0);
            let r = text_field(ui, &mut host, "host from ~/.ssh/config, or user@host", 420.0, None, &pal);
            if focus {
                r.request_focus();
                focus = false;
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                go = true;
            }
            ui.add_space(4.0);
            ui.label(RichText::new("First time? Run `sennya remote install HOST` in a terminal.").size(12.0).color(pal.dim));
            ui.add_space(16.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let connect = egui::Button::new(RichText::new("Connect").strong().color(Color32::WHITE)).fill(pal.accent).corner_radius(8).min_size(vec2(96.0, 34.0));
                if ui.add(connect).clicked() {
                    go = true;
                }
                let cancel = egui::Button::new("Cancel").fill(mix(pal.panel, pal.fg, 0.12)).corner_radius(8).min_size(vec2(96.0, 34.0));
                if ui.add(cancel).clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                    open = false;
                }
            });
        });
        if modal.should_close() {
            open = false;
        }
        if go && !host.trim().is_empty() {
            if let Err(err) = open_window(Some(host.trim())) {
                self.toast(format!("Could not open a window: {err}"));
            }
            open = false;
        }
        if open {
            self.server_prompt = Some((host, focus));
        }
    }

    fn toast_overlay(&mut self, ctx: &egui::Context) {
        let Some((text, _)) = &self.toast else { return };
        let pos = pos2(self.central_rect.center().x, self.central_rect.bottom() - 24.0);
        egui::Area::new(Id::new("toast"))
            .order(egui::Order::Tooltip)
            .pivot(Align2::CENTER_BOTTOM)
            .fixed_pos(pos)
            .interactable(false)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.label(text.as_str());
                });
            });
    }
}

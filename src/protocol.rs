//! Messages between the GUI and the daemon over a private Unix socket.
//! Framing: little-endian u32 length followed by a postcard payload.

use std::io::{self, Read, Write};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::claude::ClaudeSession;
use crate::model::{Arrange, SessionId, Workspace};

/// Bumped when messages are added. Variants are only ever appended, so older peers
/// still understand everything they knew before.
pub const PROTOCOL_VERSION: u32 = 3;
/// Oldest daemon a window can work with (it then simply misses newer features).
pub const MIN_DAEMON_VERSION: u32 = 2;
const MAX_MESSAGE: usize = 64 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Color {
    /// alacritty `NamedColor` discriminant (0..16 ansi, 256 fg, 257 bg, ...).
    Named(u16),
    Indexed(u8),
    Rgb(u8, u8, u8),
}

pub const NAMED_BG: u16 = 257;

pub mod flags {
    pub const BOLD: u16 = 1;
    pub const ITALIC: u16 = 1 << 1;
    pub const UNDERLINE: u16 = 1 << 2;
    pub const STRIKE: u16 = 1 << 3;
    pub const DIM: u16 = 1 << 4;
    pub const INVERSE: u16 = 1 << 5;
    pub const HIDDEN: u16 = 1 << 6;
    pub const SELECTED: u16 = 1 << 7;
    pub const WIDE: u16 = 1 << 8;
    pub const DOUBLE_UNDERLINE: u16 = 1 << 9;
    pub const UNDERCURL: u16 = 1 << 10;
}

/// Terminal mode bits the GUI needs for input encoding (subset of alacritty `TermMode`).
pub mod mode {
    pub const APP_CURSOR: u32 = 1 << 1;
    pub const MOUSE_REPORT_CLICK: u32 = 1 << 3;
    pub const BRACKETED_PASTE: u32 = 1 << 4;
    pub const SGR_MOUSE: u32 = 1 << 5;
    pub const MOUSE_MOTION: u32 = 1 << 6;
    pub const FOCUS_IN_OUT: u32 = 1 << 11;
    pub const ALT_SCREEN: u32 = 1 << 12;
    pub const MOUSE_DRAG: u32 = 1 << 13;
    pub const UTF8_MOUSE: u32 = 1 << 14;
    pub const ALTERNATE_SCROLL: u32 = 1 << 15;
    pub const MOUSE_MODE: u32 = MOUSE_REPORT_CLICK | MOUSE_MOTION | MOUSE_DRAG;
}

/// A span of cells sharing the same attributes, starting at column `col`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub col: u16,
    /// Width in cells.
    pub width: u16,
    pub text: String,
    pub fg: Color,
    pub bg: Color,
    pub flags: u16,
    /// 1-based index into `Frame::links`, 0 = none.
    pub link: u16,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Line {
    pub runs: Vec<Run>,
    /// The line continues on the next row (soft wrap).
    pub wrapped: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CursorShape {
    Block,
    Beam,
    Underline,
    HollowBlock,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    pub row: u16,
    pub col: u16,
    pub shape: CursorShape,
    pub wide: bool,
}

/// The visible part of a terminal.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Frame {
    pub id: SessionId,
    pub cols: u16,
    pub rows: u16,
    pub display_offset: u32,
    pub history: u32,
    pub mode: u32,
    pub cursor: Option<Cursor>,
    pub lines: Vec<Line>,
    pub links: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: SessionId,
    /// Title set by the program (OSC 0/2), if any.
    pub osc_title: String,
    pub cwd: String,
    /// Foreground process name (e.g. `vim`, `claude`).
    pub process: String,
    pub shell_pid: u32,
    pub restored: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SelectKind {
    Simple,
    Block,
    Word,
    Line,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SelectCmd {
    /// Viewport coordinates; `right` = the pointer is on the right half of the cell.
    Start { row: u16, col: u16, right: bool, kind: SelectKind },
    Update { row: u16, col: u16, right: bool },
    All,
    Clear,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScrollCmd {
    Lines(i32),
    PageUp,
    PageDown,
    Top,
    Bottom,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SearchQuery {
    pub text: String,
    pub regex: bool,
    pub case_sensitive: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SearchHit {
    pub session: SessionId,
    /// Grid line (negative = scrollback) and columns of the match.
    pub line: i32,
    pub start_col: u16,
    pub end_line: i32,
    pub end_col: u16,
    pub snippet: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ClientMsg {
    Hello { version: u32 },
    SetWorkspace(Workspace),
    CreateSession { req: u64, cwd: Option<String>, cols: u16, rows: u16 },
    CloseSession(SessionId),
    Input(SessionId, Vec<u8>),
    Resize { id: SessionId, cols: u16, rows: u16, cell_w: u16, cell_h: u16 },
    /// The sessions currently visible in this client; only these receive frames.
    Subscribe(Vec<SessionId>),
    Scroll(SessionId, ScrollCmd),
    Select(SessionId, SelectCmd),
    Copy { req: u64, id: SessionId },
    ClearScrollback(SessionId),
    /// Jump to the next match inside one terminal (selects it and scrolls to it).
    Find { id: SessionId, query: SearchQuery, backwards: bool },
    /// Search the scrollback of every terminal.
    Search { req: u64, query: SearchQuery },
    /// Select and scroll to a search hit.
    Reveal(SearchHit),
    ReloadConfig,
    /// Stop the daemon and every shell.
    Shutdown,
    // --- protocol 2 ---
    /// Open a terminal, place it and type commands into it. Answered with `Ack`.
    Spawn { req: u64, spec: SpawnSpec },
    /// Run a `[[profile]]` from the config. `vars` answers `{prompt:...}` and sets `cwd`.
    RunProfile { req: u64, name: String, vars: Vec<(String, String)>, near: Option<SessionId> },
    /// The last `lines` lines of a terminal as plain text. Answered with `Text`.
    ReadText { req: u64, id: SessionId, lines: u32 },
    Focus(SessionId),
    /// Claude Code conversations, newest first. Answered with `ClaudeHistory`.
    ClaudeHistory { req: u64, limit: u32 },
    /// Change how a page tiles (`None` = the active page).
    Arrange { page: Option<u32>, arrange: Arrange },
    /// Hide a terminal from the grid (it keeps running) or show it again.
    SetHidden { id: SessionId, hidden: bool },
    /// Save everything and exit, so the next daemon restores it. Unix sends SIGTERM instead,
    /// which older daemons understand too.
    SaveAndExit,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SpawnSpec {
    pub cwd: Option<String>,
    /// `tab`, `right`, `down`, `stack` or `page`.
    pub open: String,
    /// Place it next to this terminal instead of the focused one.
    pub near: Option<SessionId>,
    pub page: Option<String>,
    pub title: Option<String>,
    /// Lines typed into the new shell once its prompt appears (Enter is pressed after each).
    pub run: Vec<String>,
    /// Text typed afterwards without Enter, once `prefill_after` shows up on the screen.
    pub prefill: Option<String>,
    pub prefill_after: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ServerMsg {
    Hello { version: u32, pid: u32 },
    State { workspace: Workspace, sessions: Vec<SessionInfo> },
    SessionCreated { req: u64, info: SessionInfo },
    SessionUpdated(SessionInfo),
    SessionExited(SessionId),
    Frame(Box<Frame>),
    CopyText { req: u64, text: String },
    FindResult { id: SessionId, found: bool },
    SearchResults { req: u64, hits: Vec<SearchHit> },
    /// Output arrived in a session that no client is looking at.
    Activity(SessionId),
    Bell(SessionId),
    /// A program asked to set the clipboard (OSC 52).
    Clipboard(String),
    Error(String),
    // --- protocol 2 ---
    Ack { req: u64, ok: bool, message: String, session: Option<SessionId> },
    Text { req: u64, text: String },
    /// The Claude Code conversation a terminal runs (or ran last).
    /// `state`: 0 unknown, 1 ready for a prompt, 2 working, 3 needs you (permission or a choice).
    Claude { id: SessionId, session: String, title: String, running: bool, state: u8 },
    ClaudeHistory { req: u64, sessions: Vec<ClaudeSession> },
    /// Another window changed the layout: apply it but keep this window's page and mode.
    Layout { workspace: Workspace, sessions: Vec<SessionInfo> },
    // --- protocol 3 ---
    /// Only the lines that changed since the last frame this client received.
    FrameDelta(Box<FrameDelta>),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FrameDelta {
    pub id: SessionId,
    pub display_offset: u32,
    pub history: u32,
    pub mode: u32,
    pub cursor: Option<Cursor>,
    /// (row, new content)
    pub lines: Vec<(u16, Line)>,
    pub links: Vec<String>,
}

impl FrameDelta {
    /// Lines of `new` that differ from `old` (both must have the same size).
    pub fn between(old: &Frame, new: &Frame) -> Option<FrameDelta> {
        if old.cols != new.cols || old.rows != new.rows || old.lines.len() != new.lines.len() {
            return None;
        }
        let lines: Vec<(u16, Line)> = new
            .lines
            .iter()
            .enumerate()
            .filter(|(i, l)| old.lines[*i] != **l)
            .map(|(i, l)| (i as u16, l.clone()))
            .collect();
        Some(FrameDelta {
            id: new.id,
            display_offset: new.display_offset,
            history: new.history,
            mode: new.mode,
            cursor: new.cursor,
            lines,
            links: new.links.clone(),
        })
    }

    pub fn is_noop(&self, old: &Frame) -> bool {
        self.lines.is_empty()
            && self.cursor == old.cursor
            && self.mode == old.mode
            && self.display_offset == old.display_offset
            && self.history == old.history
            && self.links == old.links
    }

    pub fn apply(self, frame: &mut Frame) {
        for (row, line) in self.lines {
            if let Some(slot) = frame.lines.get_mut(row as usize) {
                *slot = line;
            }
        }
        frame.display_offset = self.display_offset;
        frame.history = self.history;
        frame.mode = self.mode;
        frame.cursor = self.cursor;
        frame.links = self.links;
    }
}

pub fn write_msg<W: Write, T: Serialize>(w: &mut W, msg: &T) -> io::Result<()> {
    let body = postcard::to_stdvec(msg).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    w.write_all(&(body.len() as u32).to_le_bytes())?;
    w.write_all(&body)
}

pub fn read_msg<R: Read, T: DeserializeOwned>(r: &mut R) -> io::Result<T> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_MESSAGE {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "message too large"));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    postcard::from_bytes(&body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ViewMode;

    #[test]
    fn roundtrip() {
        let msgs = vec![
            ClientMsg::SetWorkspace(Workspace::new(ViewMode::Grid)),
            ClientMsg::Input(3, b"ls\r".to_vec()),
            ClientMsg::Select(1, SelectCmd::Start { row: 1, col: 2, right: true, kind: SelectKind::Word }),
        ];
        let mut buf = Vec::new();
        for m in &msgs {
            write_msg(&mut buf, m).unwrap();
        }
        let mut r = &buf[..];
        for m in &msgs {
            let got: ClientMsg = read_msg(&mut r).unwrap();
            assert_eq!(&got, m);
        }
    }
}

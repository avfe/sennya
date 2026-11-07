//! The session daemon. It owns every PTY so terminals survive the GUI being closed,
//! killed or crashing, and it snapshots the workspace + scrollback to disk so they
//! come back even after the daemon itself dies (reboot, `kill -9`).

mod procinfo;
pub mod session;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufReader, BufWriter, Write};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use alacritty_terminal::event::Event;
use alacritty_terminal::vte::ansi::Rgb as VRgb;
use serde::{Deserialize, Serialize};

use crate::claude;
use crate::config::{Config, ModeCfg, Step};
use crate::model::{Dir, SessionId, ViewMode, Workspace};
use crate::paths;
use crate::protocol::{read_msg, write_msg, ClientMsg, Frame, FrameDelta, ServerMsg, SpawnSpec, PROTOCOL_VERSION};
use crate::sys::{self, UnixListener, UnixStream};
use crate::template::{self, Template};
use session::{Session, SpawnOpts};

macro_rules! log {
    ($($arg:tt)*) => {{
        let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        eprintln!("[{secs}] {}", format!($($arg)*));
    }};
}

#[derive(Serialize, Deserialize)]
struct SavedState {
    version: u32,
    saved_at: u64,
    workspace: Workspace,
    sessions: Vec<SavedSession>,
    next_session: u64,
}

#[derive(Serialize, Deserialize)]
struct SavedSession {
    id: SessionId,
    cwd: String,
    cols: u16,
    rows: u16,
    #[serde(default)]
    claude: String,
    #[serde(default)]
    claude_title: String,
    #[serde(default)]
    claude_running: bool,
    #[serde(default)]
    claude_flags: Vec<String>,
    #[serde(default)]
    claude_hibernated: bool,
}

struct Client {
    tx: SyncSender<ServerMsg>,
    subs: HashSet<SessionId>,
    /// Protocol version the client announced.
    version: u32,
    /// Last frame each subscribed terminal was sent to this client (base for deltas).
    last: HashMap<SessionId, Frame>,
}

struct Inner {
    sessions: HashMap<SessionId, Arc<Session>>,
    workspace: Workspace,
    clients: HashMap<u64, Client>,
    next_session: u64,
    next_client: u64,
    ws_dirty: bool,
}

pub struct Daemon {
    inner: Mutex<Inner>,
    dirty: Mutex<HashSet<SessionId>>,
    dirty_cv: Condvar,
    events: mpsc::Sender<(SessionId, Event)>,
    config: RwLock<Config>,
    claude: Mutex<claude::Index>,
    started: Instant,
    /// Serialises writes of state.json and scrollback files.
    save_lock: Mutex<()>,
    /// Set by `sennya kill`: nothing may be written any more.
    shutting_down: std::sync::atomic::AtomicBool,
    /// Why the config file could not be used (the previous good config stays active).
    config_error: Mutex<Option<String>>,
    config_mtime: Mutex<Option<SystemTime>>,
}

fn config_mtime() -> Option<SystemTime> {
    fs::metadata(paths::config_file()).and_then(|m| m.modified()).ok()
}

/// Environment variables leaked by whatever terminal launched us; shells must not inherit them.
const SCRUB_PREFIXES: &[&str] = &[
    "ITERM_", "TMUX", "ALACRITTY_", "KITTY_", "WEZTERM_", "GHOSTTY_", "VSCODE_", "WARP_",
    "CLAUDE_CODE_", "CLAUDECODE", "TERM_SESSION_ID", "TERMINAL_EMULATOR", "STY", "WINDOWID",
    "__CFBundleIdentifier", "OLDPWD", "TERM_PROGRAM",
];

fn scrub_env() {
    for (key, _) in std::env::vars_os() {
        let k = key.to_string_lossy();
        if SCRUB_PREFIXES.iter().any(|p| k.starts_with(p)) {
            std::env::remove_var(&key);
        }
    }
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(any(target_os = "macos", target_os = "freebsd", target_os = "openbsd", target_os = "netbsd"))]
fn peer_is_same_user(stream: &UnixStream) -> bool {
    use std::os::fd::AsRawFd;
    let (mut uid, mut gid) = (0, 0);
    unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) == 0 && uid == libc::geteuid() }
}

#[cfg(target_os = "linux")]
fn peer_is_same_user(stream: &UnixStream) -> bool {
    use std::os::fd::AsRawFd;
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let ok = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut _ as *mut libc::c_void,
            &mut len,
        ) == 0
    };
    ok && cred.uid == unsafe { libc::geteuid() }
}

/// Windows checks access to the socket file itself, which lives in the user's profile.
#[cfg(windows)]
fn peer_is_same_user(_stream: &UnixStream) -> bool {
    true
}

pub fn run() -> anyhow::Result<()> {
    paths::ensure_private_dir(&paths::data_dir())?;
    paths::ensure_private_dir(&paths::scrollback_dir())?;
    paths::ensure_socket_dir()?;
    // Only one daemon at a time, even when several clients start one at the same moment.
    let Some(_lock) = sys::try_lock(&paths::data_dir().join("daemon.lock"))? else {
        log!("another daemon is already running");
        return Ok(());
    };
    let sock = paths::socket_path();
    if UnixStream::connect(&sock).is_ok() {
        log!("daemon already running");
        return Ok(());
    }
    let _ = fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock)?;
    sys::make_private_file(&sock)?;

    scrub_env();
    if let Some(home) = dirs::home_dir() {
        let _ = std::env::set_current_dir(home);
    }
    let (config, config_error) = Config::load();
    if let Some(err) = &config_error {
        log!("{err}");
    }
    match crate::cli::sync_skill() {
        Ok(Some(path)) => log!("installed the Claude Code skill at {}", path.display()),
        Ok(None) => {}
        Err(err) => log!("could not install the Claude Code skill: {err}"),
    }
    let mode = match config.ui.default_mode {
        ModeCfg::Tabs => ViewMode::Tabs,
        ModeCfg::Grid => ViewMode::Grid,
    };
    let (events_tx, events_rx) = mpsc::channel();
    let daemon = Arc::new(Daemon {
        inner: Mutex::new(Inner {
            sessions: HashMap::new(),
            workspace: Workspace::new(mode),
            clients: HashMap::new(),
            next_session: 0,
            next_client: 0,
            ws_dirty: false,
        }),
        dirty: Mutex::new(HashSet::new()),
        dirty_cv: Condvar::new(),
        events: events_tx,
        config: RwLock::new(config),
        claude: Mutex::new(claude::Index::default()),
        started: Instant::now(),
        save_lock: Mutex::new(()),
        shutting_down: std::sync::atomic::AtomicBool::new(false),
        config_error: Mutex::new(config_error),
        config_mtime: Mutex::new(config_mtime()),
    });
    daemon.restore();
    log!("daemon started, pid {}", std::process::id());

    let d = daemon.clone();
    thread::Builder::new().name("dispatch".into()).spawn(move || d.dispatch(events_rx))?;
    let d = daemon.clone();
    thread::Builder::new().name("pump".into()).spawn(move || d.pump())?;
    let d = daemon.clone();
    thread::Builder::new().name("housekeeping".into()).spawn(move || d.housekeeping())?;
    #[cfg(unix)]
    let d = daemon.clone();
    #[cfg(unix)]
    let mut signals = signal_hook::iterator::Signals::new([
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGHUP,
    ])?;
    #[cfg(unix)]
    thread::Builder::new().name("signals".into()).spawn(move || {
        if let Some(sig) = signals.forever().next() {
            log!("signal {sig}: saving and exiting");
            d.save_all();
            let _ = fs::remove_file(paths::socket_path());
            std::process::exit(0);
        }
    })?;

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let d = daemon.clone();
                let _ = thread::Builder::new().name("client".into()).spawn(move || d.serve(stream));
            }
            Err(err) => log!("accept failed: {err}"),
        }
    }
    Ok(())
}

impl Daemon {
    fn session(&self, id: SessionId) -> Option<Arc<Session>> {
        self.inner.lock().unwrap().sessions.get(&id).cloned()
    }

    fn mark_dirty(&self, id: SessionId) {
        self.dirty.lock().unwrap().insert(id);
        self.dirty_cv.notify_one();
    }

    fn broadcast(&self, msg: ServerMsg) {
        let inner = self.inner.lock().unwrap();
        for client in inner.clients.values() {
            let _ = client.tx.try_send(msg.clone());
        }
    }

    fn state_msg(&self) -> ServerMsg {
        let mut inner = self.inner.lock().unwrap();
        let live: HashSet<SessionId> = inner.sessions.keys().copied().collect();
        inner.workspace.sanitize(&live);
        self.state_of(&inner)
    }

    fn state_of(&self, inner: &Inner) -> ServerMsg {
        let workspace = inner.workspace.clone();
        let sessions = workspace
            .all_sessions()
            .iter()
            .filter_map(|id| inner.sessions.get(id).map(|s| s.info()))
            .collect();
        ServerMsg::State { workspace, sessions }
    }

    fn spawn_session(&self, id: SessionId, cwd: Option<String>, cols: u16, rows: u16, restore: Option<Vec<u8>>) -> anyhow::Result<Arc<Session>> {
        let config = self.config.read().unwrap().clone();
        let session = Session::spawn(
            SpawnOpts { id, cwd, cols, rows, restore, config: &config },
            self.events.clone(),
        )?;
        self.inner.lock().unwrap().sessions.insert(id, session.clone());
        Ok(session)
    }

    fn serve(self: Arc<Self>, stream: UnixStream) {
        if !peer_is_same_user(&stream) {
            log!("rejected connection from another user");
            return;
        }
        let Ok(write_half) = stream.try_clone() else { return };
        let (tx, rx) = mpsc::sync_channel::<ServerMsg>(1024);
        thread::spawn(move || writer(write_half, rx));
        let client_id = {
            let mut inner = self.inner.lock().unwrap();
            inner.next_client += 1;
            let id = inner.next_client;
            inner.clients.insert(id, Client { tx: tx.clone(), subs: HashSet::new(), version: 0, last: HashMap::new() });
            id
        };
        let _ = tx.send(ServerMsg::Hello { version: PROTOCOL_VERSION, pid: std::process::id() });
        let _ = tx.send(self.state_msg());
        let sessions: Vec<Arc<Session>> = self.inner.lock().unwrap().sessions.values().cloned().collect();
        for s in sessions {
            if let Some(msg) = claude_msg(&s) {
                let _ = tx.send(msg);
            }
        }

        let mut reader = BufReader::with_capacity(64 * 1024, stream);
        while let Ok(msg) = read_msg::<_, ClientMsg>(&mut reader) {
            self.handle(client_id, &tx, msg);
        }
        let mut inner = self.inner.lock().unwrap();
        inner.clients.remove(&client_id);
        drop(inner);
        self.update_watched();
    }

    fn update_watched(&self) {
        let inner = self.inner.lock().unwrap();
        for (id, s) in &inner.sessions {
            let watched = inner.clients.values().any(|c| c.subs.contains(id));
            if watched {
                s.activity_sent.store(false, Ordering::Relaxed);
            }
            s.watched.store(watched, Ordering::Relaxed);
        }
    }

    fn handle(self: &Arc<Self>, client: u64, tx: &SyncSender<ServerMsg>, msg: ClientMsg) {
        match msg {
            ClientMsg::Hello { version } => {
                if let Some(c) = self.inner.lock().unwrap().clients.get_mut(&client) {
                    c.version = version;
                }
            }
            ClientMsg::SetWorkspace(mut ws) => {
                let state = {
                    let mut inner = self.inner.lock().unwrap();
                    let live: HashSet<SessionId> = inner.sessions.keys().copied().collect();
                    ws.sanitize(&live);
                    if inner.workspace == ws {
                        return;
                    }
                    inner.workspace = ws;
                    inner.ws_dirty = true;
                    let others = inner.clients.len() > 1;
                    others.then(|| self.state_of(&inner))
                };
                // Other windows pick up layout changes; each keeps its own page and mode.
                if let Some(ServerMsg::State { workspace, sessions }) = state {
                    let msg = ServerMsg::Layout { workspace, sessions };
                    let inner = self.inner.lock().unwrap();
                    for (id, c) in &inner.clients {
                        if *id != client {
                            let _ = c.tx.try_send(msg.clone());
                        }
                    }
                }
            }
            ClientMsg::CreateSession { req, cwd, cols, rows } => {
                let id = {
                    let mut inner = self.inner.lock().unwrap();
                    inner.next_session += 1;
                    inner.next_session
                };
                match self.spawn_session(id, cwd, cols, rows, None) {
                    Ok(s) => {
                        let _ = tx.send(ServerMsg::SessionCreated { req, info: s.info() });
                    }
                    Err(err) => {
                        log!("spawn failed: {err}");
                        let _ = tx.send(ServerMsg::Error(format!("Failed to start shell: {err}")));
                    }
                }
            }
            ClientMsg::CloseSession(id) => {
                if let Some(s) = self.remove_session(id) {
                    s.shutdown();
                }
            }
            ClientMsg::Input(id, bytes) => {
                if let Some(s) = self.session(id) {
                    s.input(bytes);
                }
            }
            ClientMsg::Resize { id, cols, rows, cell_w, cell_h } => {
                if let Some(s) = self.session(id) {
                    if s.resize(cols, rows, cell_w, cell_h) {
                        self.mark_dirty(id);
                    }
                }
            }
            ClientMsg::Subscribe(ids) => {
                {
                    let mut inner = self.inner.lock().unwrap();
                    if let Some(c) = inner.clients.get_mut(&client) {
                        c.subs = ids.iter().copied().collect();
                        // Start over with full frames.
                        c.last.clear();
                    }
                }
                self.update_watched();
                for id in &ids {
                    if let Some(s) = self.session(*id) {
                        self.wake_claude(&s);
                    }
                }
                let mut dirty = self.dirty.lock().unwrap();
                dirty.extend(ids);
                self.dirty_cv.notify_one();
            }
            ClientMsg::Scroll(id, cmd) => {
                if let Some(s) = self.session(id) {
                    s.scroll(cmd);
                    self.mark_dirty(id);
                }
            }
            ClientMsg::Select(id, cmd) => {
                if let Some(s) = self.session(id) {
                    s.select(cmd);
                    self.mark_dirty(id);
                }
            }
            ClientMsg::Copy { req, id } => {
                let text = self.session(id).map(|s| s.selection_text()).unwrap_or_default();
                let _ = tx.send(ServerMsg::CopyText { req, text });
            }
            ClientMsg::ClearScrollback(id) => {
                if let Some(s) = self.session(id) {
                    s.clear_scrollback();
                    self.mark_dirty(id);
                }
            }
            ClientMsg::Find { id, query, backwards } => {
                if let Some(s) = self.session(id) {
                    let found = s.find(&query, backwards);
                    self.mark_dirty(id);
                    let _ = tx.send(ServerMsg::FindResult { id, found });
                }
            }
            ClientMsg::Search { req, query } => {
                let sessions: Vec<Arc<Session>> = {
                    let inner = self.inner.lock().unwrap();
                    let mut order = inner.workspace.all_sessions();
                    let mut rest: Vec<SessionId> =
                        inner.sessions.keys().copied().filter(|id| !order.contains(id)).collect();
                    rest.sort_unstable();
                    order.extend(rest);
                    order.iter().filter_map(|id| inner.sessions.get(id).cloned()).collect()
                };
                let tx = tx.clone();
                thread::spawn(move || {
                    let mut hits = Vec::new();
                    for s in sessions {
                        hits.extend(s.search(&query, 40));
                        if hits.len() > 600 {
                            break;
                        }
                    }
                    let _ = tx.send(ServerMsg::SearchResults { req, hits });
                });
            }
            ClientMsg::Reveal(hit) => {
                if let Some(s) = self.session(hit.session) {
                    s.reveal(&hit);
                    self.mark_dirty(hit.session);
                }
            }
            ClientMsg::ReloadConfig => {
                if let Some(err) = self.reload_config() {
                    let _ = tx.send(ServerMsg::Error(err));
                }
            }
            ClientMsg::Spawn { req, spec } => {
                let mut steps: Vec<Step> = spec.run.iter().cloned().map(Step::Run).collect();
                if let Some(text) = spec.prefill.clone().filter(|t| !t.is_empty()) {
                    steps.push(Step::Detailed {
                        run: text,
                        wait_for: None,
                        wait_text: spec.prefill_after.clone(),
                        delay_ms: 300,
                        timeout_ms: Some(30_000),
                        enter: Some(false),
                    });
                }
                let reply = match self.spawn_placed(&spec, steps) {
                    Ok(id) => ServerMsg::Ack { req, ok: true, message: format!("opened terminal {id}"), session: Some(id) },
                    Err(err) => ServerMsg::Ack { req, ok: false, message: err.to_string(), session: None },
                };
                let _ = tx.send(reply);
            }
            ClientMsg::RunProfile { req, name, vars, near } => {
                let reply = match self.run_profile(&name, &vars, near) {
                    Ok(id) => ServerMsg::Ack { req, ok: true, message: format!("profile {name} opened terminal {id}"), session: Some(id) },
                    Err(err) => ServerMsg::Ack { req, ok: false, message: err.to_string(), session: None },
                };
                let _ = tx.send(reply);
            }
            ClientMsg::ReadText { req, id, lines } => {
                let text = self.session(id).map(|s| s.text(lines as usize)).unwrap_or_default();
                let _ = tx.send(ServerMsg::Text { req, text });
            }
            ClientMsg::Focus(id) => {
                {
                    let mut inner = self.inner.lock().unwrap();
                    inner.workspace.focus(id);
                    inner.ws_dirty = true;
                }
                self.broadcast(self.state_msg());
            }
            ClientMsg::ClaudeHistory { req, limit } => {
                let mut sessions = self.claude.lock().unwrap().all(limit.clamp(1, 2000) as usize);
                let inner = self.inner.lock().unwrap();
                for s in &mut sessions {
                    if let Some((sid, _)) = inner.sessions.iter().find(|(_, x)| x.meta.lock().unwrap().claude == s.id) {
                        s.terminal = inner.workspace.names.get(sid).cloned().unwrap_or_else(|| format!("terminal {sid}"));
                    }
                }
                drop(inner);
                let _ = tx.send(ServerMsg::ClaudeHistory { req, sessions });
            }
            ClientMsg::Arrange { page, arrange } => {
                {
                    let mut inner = self.inner.lock().unwrap();
                    let page = page.map(|p| p as usize).unwrap_or(inner.workspace.active_page);
                    inner.workspace.set_arrange(page, arrange);
                    inner.ws_dirty = true;
                }
                self.broadcast(self.state_msg());
            }
            ClientMsg::SetHidden { id, hidden } => {
                {
                    let mut inner = self.inner.lock().unwrap();
                    if hidden {
                        inner.workspace.hide(id);
                    } else {
                        inner.workspace.unhide(id);
                    }
                    inner.ws_dirty = true;
                }
                self.broadcast(self.state_msg());
            }
            ClientMsg::SaveAndExit => {
                log!("restart requested: saving and exiting");
                self.save_all();
                let _ = fs::remove_file(paths::socket_path());
                std::process::exit(0);
            }
            ClientMsg::Shutdown => {
                log!("shutdown requested: closing every session");
                self.shutting_down.store(true, Ordering::SeqCst);
                let _guard = self.save_lock.lock().unwrap();
                let _ = fs::remove_file(paths::state_file());
                let _ = fs::remove_dir_all(paths::scrollback_dir());
                let _ = fs::remove_file(paths::socket_path());
                std::process::exit(0);
            }
        }
    }

    /// Open a terminal, place it in the workspace, and type `steps` into it.
    fn spawn_placed(self: &Arc<Self>, spec: &SpawnSpec, steps: Vec<Step>) -> anyhow::Result<SessionId> {
        let (id, cols, rows) = {
            let mut inner = self.inner.lock().unwrap();
            inner.next_session += 1;
            let size = inner
                .workspace
                .focused()
                .and_then(|f| inner.sessions.get(&f))
                .map(|s| {
                    let m = s.meta.lock().unwrap();
                    (m.cols, m.rows)
                })
                .unwrap_or((100, 30));
            (inner.next_session, size.0, size.1)
        };
        let cwd = spec.cwd.as_deref().filter(|c| !c.is_empty()).map(template::expand_tilde);
        let config = self.config.read().unwrap().clone();
        let session = Session::spawn(SpawnOpts { id, cwd, cols, rows, restore: None, config: &config }, self.events.clone())?;
        {
            // Register and place in one step: a concurrent `State` would otherwise adopt the
            // new terminal on the active page before it reaches its intended place.
            let mut inner = self.inner.lock().unwrap();
            inner.sessions.insert(id, session.clone());
            let ws = &mut inner.workspace;
            if let Some(near) = spec.near.filter(|n| ws.page_of(*n).is_some()) {
                ws.focus(near);
            }
            let active = ws.active_page;
            let main = ws.focused();
            match spec.open.as_str() {
                "stack" if main.is_some() => {
                    ws.add_session(id, active, None, Dir::Horizontal);
                    ws.stack_beside(main.unwrap(), id);
                }
                "page" => {
                    let page = ws.new_page();
                    if let Some(name) = spec.page.as_ref().filter(|n| !n.is_empty()) {
                        ws.pages[page].name = name.clone();
                    }
                    ws.add_session(id, page, None, Dir::Horizontal);
                }
                "right" => ws.add_session(id, active, Some(Dir::Horizontal), Dir::Horizontal),
                "down" => ws.add_session(id, active, Some(Dir::Vertical), Dir::Vertical),
                _ => ws.add_session(id, active, None, Dir::Horizontal),
            }
            if let Some(title) = spec.title.as_ref().filter(|t| !t.is_empty()) {
                ws.names.insert(id, title.clone());
            }
            inner.ws_dirty = true;
        }
        self.broadcast(self.state_msg());
        if !steps.is_empty() {
            self.type_steps(session, steps);
        }
        Ok(id)
    }

    fn run_profile(self: &Arc<Self>, name: &str, vars: &[(String, String)], near: Option<SessionId>) -> anyhow::Result<SessionId> {
        let config = self.config.read().unwrap().clone();
        let Some(profile) = config.profile(name) else {
            if let Some(err) = self.config_error.lock().unwrap().clone() {
                anyhow::bail!("no profile named {name:?}: {err}");
            }
            let known: Vec<&str> = config.profiles.iter().map(|p| p.name.as_str()).collect();
            anyhow::bail!("no profile named {name:?} (known: {})", known.join(", "));
        };
        let missing: Vec<String> = template::prompts(profile.texts())
            .into_iter()
            .filter(|label| !vars.iter().any(|(k, _)| *k == format!("prompt:{label}")))
            .collect();
        if !missing.is_empty() {
            anyhow::bail!("profile {name:?} needs values for: {}", missing.join(", "));
        }
        let t = Template::new(vars);
        let spec = SpawnSpec {
            prefill: None,
            prefill_after: None,
            cwd: Some(t.expand(&profile.cwd)).filter(|c| !c.is_empty()).or_else(|| {
                vars.iter().find(|(k, _)| k == "cwd").map(|(_, v)| v.clone())
            }),
            open: profile.open.clone(),
            near,
            page: Some(t.expand(&profile.page)),
            title: Some(t.expand(&profile.title)),
            run: Vec::new(),
        };
        let steps = profile
            .steps
            .iter()
            .map(|step| match step {
                Step::Run(run) => Step::Run(t.expand(run)),
                Step::Detailed { run, wait_for, wait_text, delay_ms, timeout_ms, enter } => Step::Detailed {
                    run: t.expand(run),
                    wait_for: wait_for.clone(),
                    wait_text: wait_text.clone(),
                    delay_ms: *delay_ms,
                    timeout_ms: *timeout_ms,
                    enter: *enter,
                },
            })
            .collect();
        self.spawn_placed(&spec, steps)
    }

    /// Type commands into a terminal, waiting for the shell (and for prompts) between them.
    fn type_steps(self: &Arc<Self>, session: Arc<Session>, steps: Vec<Step>) {
        let daemon = self.clone();
        thread::spawn(move || {
            let alive = |d: &Daemon| d.session(session.id).is_some();
            let wait = |cond: &dyn Fn() -> bool, limit: Duration| -> bool {
                let start = Instant::now();
                while start.elapsed() < limit {
                    if cond() {
                        return true;
                    }
                    thread::sleep(Duration::from_millis(50));
                }
                false
            };
            for (i, step) in steps.iter().enumerate() {
                if !alive(&daemon) {
                    return;
                }
                let (run, wait_for, wait_text, delay, timeout, enter) = match step {
                    Step::Run(run) => (run.as_str(), None, None, 0, 60_000, true),
                    Step::Detailed { run, wait_for, wait_text, delay_ms, timeout_ms, enter } => (
                        run.as_str(),
                        wait_for.as_deref(),
                        wait_text.as_deref(),
                        *delay_ms,
                        timeout_ms.unwrap_or(60_000),
                        enter.unwrap_or(true),
                    ),
                };
                if i == 0 {
                    if session.spawned.elapsed() < Duration::from_secs(10) {
                        // A shell that just started (restored scrollback already shows text):
                        // wait until it has printed something and then gone quiet for a moment,
                        // so the rc files are done and typeahead is not lost.
                        let born = session.spawned.duration_since(daemon.started).as_millis() as u64;
                        let now = || daemon.started.elapsed().as_millis() as u64;
                        wait(
                            &|| {
                                let last = session.last_output.load(Ordering::Relaxed);
                                last > born && now().saturating_sub(last) >= 400
                            },
                            Duration::from_secs(8),
                        );
                    } else {
                        wait(&|| session.has_text(), Duration::from_secs(5));
                    }
                    thread::sleep(Duration::from_millis(150));
                }
                if let Some(text) = wait_text.filter(|t| !t.is_empty()) {
                    // `@claude-ready`: Claude Code's prompt box is up and idle.
                    let ok = if text == "@claude-ready" {
                        wait(&|| session.claude_state() == session::CLAUDE_READY, Duration::from_millis(timeout))
                    } else {
                        wait(&|| session.text(500).contains(text), Duration::from_millis(timeout))
                    };
                    if !ok {
                        daemon.broadcast(ServerMsg::Error(format!("Stopped typing commands: \"{text}\" never appeared")));
                        return;
                    }
                }
                if let Some(pattern) = wait_for.map(str::trim).filter(|p| !p.is_empty()) {
                    let alternatives: Vec<&str> = pattern.split('|').map(str::trim).filter(|p| !p.is_empty()).collect();
                    let ok = wait(
                        &|| {
                            let line = session.cursor_line();
                            alternatives.iter().any(|a| line.ends_with(a))
                        },
                        Duration::from_millis(timeout),
                    );
                    if !ok {
                        daemon.broadcast(ServerMsg::Error(format!(
                            "Stopped typing commands: no \"{pattern}\" prompt after {}s",
                            timeout / 1000
                        )));
                        return;
                    }
                }
                thread::sleep(Duration::from_millis(delay));
                let mut bytes = run.as_bytes().to_vec();
                if enter {
                    bytes.push(b'\r');
                }
                session.write(bytes);
                // Let the echo land so the next wait does not match the old prompt.
                thread::sleep(Duration::from_millis(300));
            }
        });
    }

    /// Reopen a hibernated chat in its terminal.
    fn wake_claude(self: &Arc<Self>, s: &Arc<Session>) {
        let line = {
            let mut m = s.meta.lock().unwrap();
            if !m.claude_hibernated || m.claude.is_empty() || !m.process.is_empty() {
                return;
            }
            m.claude_hibernated = false;
            let mut parts = vec!["claude".to_string(), "--resume".into(), crate::template::local_word(&m.claude)];
            parts.extend(m.claude_flags.iter().map(|f| crate::template::local_word(f)));
            // Claude finds a chat through its project folder, so resume from there.
            let chat_cwd = self.claude.lock().unwrap().find(&m.claude).map(|c| c.cwd).filter(|c| !c.is_empty());
            match chat_cwd {
                Some(dir) if dir != m.cwd => crate::template::cd_then(&dir, &parts.join(" ")),
                _ => parts.join(" "),
            }
        };
        self.type_steps(s.clone(), vec![Step::Run(line)]);
        if let Some(msg) = claude_msg(s) {
            self.broadcast(msg);
        }
    }

    /// Close Claude in terminals nobody is looking at once it has idled long enough.
    fn hibernate_idle(&self, sessions: &[Arc<Session>]) {
        // SENNYA_HIBERNATE_SECS overrides the minutes from the config (used by the tests).
        let after_secs = std::env::var("SENNYA_HIBERNATE_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(self.config.read().unwrap().claude.hibernate_after_mins * 60);
        if after_secs == 0 {
            return;
        }
        for s in sessions {
            let (due, fg_pid) = {
                let mut m = s.meta.lock().unwrap();
                // A `/exit` that did not end Claude within a minute is forgotten.
                if m.claude_exit_requested.is_some_and(|t| t.elapsed() > Duration::from_secs(60)) {
                    log!("Claude in terminal {} did not exit; it stays awake", s.id);
                    m.claude_exit_requested = None;
                }
                let due = m.claude_running
                    && m.claude_exit_requested.is_none()
                    && m.process == "claude"
                    && !m.claude.is_empty()
                    && m.claude_state == session::CLAUDE_READY
                    && m.claude_idle_since.is_some_and(|t| t.elapsed() >= Duration::from_secs(after_secs));
                (due, m.fg_pid)
            };
            if !due || s.watched.load(Ordering::Relaxed) {
                continue;
            }
            // Never while anything is still being printed: a working Claude redraws its spinner
            // every second, so this holds even if its screen is not recognised.
            let now = self.started.elapsed().as_millis() as u64;
            let quiet_ms = now.saturating_sub(s.last_output.load(Ordering::Relaxed));
            if quiet_ms < after_secs * 1000 || !s.claude_input_empty() {
                continue;
            }
            // Background work Claude started (a dev server, a long build) runs in child shells,
            // which `/exit` would stop.
            let busy = procinfo::children(fg_pid)
                .into_iter()
                .any(|c| procinfo::display_name(c).is_some_and(|n| procinfo::is_shell(&n)));
            if busy {
                continue;
            }
            s.meta.lock().unwrap().claude_exit_requested = Some(Instant::now());
            s.write(b"/exit\r".to_vec());
            log!("asked Claude in terminal {} to exit to save memory", s.id);
        }
    }

    /// Follow Claude Code running in terminals so its conversation can be resumed later.
    ///
    /// A chat is attached to a terminal when its Claude starts (from `--resume ID`, or the
    /// newest transcript in that folder that no other running Claude has) and stays attached
    /// for that run, so two Claudes in one folder never swap or share chats.
    fn track_claude(&self, sessions: &[Arc<Session>]) {
        let mut taken: HashSet<String> = sessions
            .iter()
            .filter_map(|s| {
                let m = s.meta.lock().unwrap();
                (m.claude_running && !m.claude.is_empty()).then(|| m.claude.clone())
            })
            .collect();
        for s in sessions {
            let (running, fg_pid, cwd) = {
                let m = s.meta.lock().unwrap();
                (m.process == "claude", m.fg_pid, m.cwd.clone())
            };
            let mut changed = false;
            if running {
                let (since, current) = {
                    let mut m = s.meta.lock().unwrap();
                    if !m.claude_running {
                        // A new run: forget the previous run's chat and options.
                        let args = procinfo::args(fg_pid).unwrap_or_default();
                        m.claude_running = true;
                        m.claude_since = Some(SystemTime::now() - Duration::from_secs(10));
                        m.claude_flags = procinfo::claude_flags(&args);
                        m.claude = procinfo::claude_resume_id(&args).unwrap_or_default();
                        m.claude_title.clear();
                        m.claude_hibernated = false;
                        m.claude_exit_requested = None;
                        if !m.claude.is_empty() {
                            taken.insert(m.claude.clone());
                        }
                        changed = true;
                    }
                    (m.claude_since.unwrap_or_else(SystemTime::now), m.claude.clone())
                };
                let cwd = procinfo::cwd(fg_pid).unwrap_or(cwd);
                let found = if current.is_empty() {
                    let excluded: Vec<String> = taken.iter().cloned().collect();
                    self.claude.lock().unwrap().newest_in(&cwd, since, &excluded)
                } else {
                    self.claude.lock().unwrap().find(&current)
                };
                if let Some(cs) = found {
                    let mut m = s.meta.lock().unwrap();
                    if m.claude.is_empty() {
                        m.claude = cs.id.clone();
                        taken.insert(cs.id.clone());
                        changed = true;
                    }
                    if m.claude == cs.id && m.claude_title != cs.title {
                        m.claude_title = cs.title;
                        changed = true;
                    }
                }
            } else {
                let mut m = s.meta.lock().unwrap();
                if m.claude_running {
                    m.claude_running = false;
                    m.claude_since = None;
                    m.claude_state = session::CLAUDE_UNKNOWN;
                    // Asleep only when Claude really quit after our `/exit`.
                    if m.claude_exit_requested.take().is_some() && !m.claude.is_empty() {
                        m.claude_hibernated = true;
                        log!("Claude in terminal {} is asleep", s.id);
                    }
                    changed = true;
                }
            }
            if changed {
                if let Some(msg) = claude_msg(s) {
                    self.broadcast(msg);
                }
                self.inner.lock().unwrap().ws_dirty = true;
            }
        }
    }

    /// Re-read the config; on errors the previous config stays active.
    fn reload_config(&self) -> Option<String> {
        *self.config_mtime.lock().unwrap() = config_mtime();
        let (config, err) = Config::load();
        if let Some(err) = err {
            log!("{err}");
            *self.config_error.lock().unwrap() = Some(err.clone());
            return Some(err);
        }
        *self.config_error.lock().unwrap() = None;
        let term_cfg = session::term_config(&config);
        let sessions: Vec<_> = self.inner.lock().unwrap().sessions.values().cloned().collect();
        for s in sessions {
            s.term.lock().set_options(term_cfg.clone());
        }
        *self.config.write().unwrap() = config;
        None
    }

    /// Forget a session (closed by the user or its shell exited).
    fn remove_session(&self, id: SessionId) -> Option<Arc<Session>> {
        let _guard = self.save_lock.lock().unwrap();
        let session = {
            let mut inner = self.inner.lock().unwrap();
            let s = inner.sessions.remove(&id);
            inner.workspace.remove_session(id);
            inner.ws_dirty = true;
            s
        };
        let _ = fs::remove_file(paths::scrollback_file(id));
        drop(_guard);
        self.broadcast(ServerMsg::SessionExited(id));
        session
    }

    fn theme_color(&self, index: usize) -> VRgb {
        let theme = self.config.read().unwrap().theme();
        let c = match index {
            0..=255 => theme.indexed(index as u8),
            256 => theme.foreground,
            257 => theme.background,
            258 => theme.cursor,
            _ => theme.foreground,
        };
        VRgb { r: c[0], g: c[1], b: c[2] }
    }

    fn dispatch(self: Arc<Self>, rx: Receiver<(SessionId, Event)>) {
        for (id, event) in rx {
            let Some(s) = self.session(id) else { continue };
            match event {
                Event::Wakeup => {
                    s.unsaved.store(true, Ordering::Relaxed);
                    s.last_output.store(self.started.elapsed().as_millis() as u64, Ordering::Relaxed);
                    if s.watched.load(Ordering::Relaxed) {
                        self.mark_dirty(id);
                    } else if !s.activity_sent.swap(true, Ordering::Relaxed) {
                        self.broadcast(ServerMsg::Activity(id));
                    }
                }
                Event::Title(title) => {
                    s.meta.lock().unwrap().osc_title = title;
                    self.broadcast(ServerMsg::SessionUpdated(s.info()));
                }
                Event::ResetTitle => {
                    s.meta.lock().unwrap().osc_title.clear();
                    self.broadcast(ServerMsg::SessionUpdated(s.info()));
                }
                Event::ClipboardStore(_, text) => {
                    if self.config.read().unwrap().security.osc52_copy {
                        self.broadcast(ServerMsg::Clipboard(text));
                    }
                }
                // Programs never get to read the clipboard.
                Event::ClipboardLoad(..) => {}
                Event::ColorRequest(index, format) => {
                    let set = s.term.lock().colors()[index];
                    let rgb = set.unwrap_or_else(|| self.theme_color(index));
                    s.write(format(rgb).into_bytes());
                }
                Event::PtyWrite(text) => s.write(text.into_bytes()),
                Event::TextAreaSizeRequest(format) => s.write(format(s.window_size()).into_bytes()),
                Event::Bell => self.broadcast(ServerMsg::Bell(id)),
                Event::Exit | Event::ChildExit(_) => {
                    self.remove_session(id);
                }
                Event::MouseCursorDirty | Event::CursorBlinkingChange => {}
            }
        }
    }

    /// Sends fresh frames of changed, visible terminals. Coalesces bursts of output
    /// into at most ~140 frames per second per terminal.
    fn pump(self: Arc<Self>) {
        loop {
            let ids: Vec<SessionId> = {
                let mut dirty = self.dirty.lock().unwrap();
                while dirty.is_empty() {
                    dirty = self.dirty_cv.wait(dirty).unwrap();
                }
                dirty.drain().collect()
            };
            let sessions: Vec<Arc<Session>> = {
                let inner = self.inner.lock().unwrap();
                ids.iter()
                    .filter(|id| inner.clients.values().any(|c| c.subs.contains(id)))
                    .filter_map(|id| inner.sessions.get(id).cloned())
                    .collect()
            };
            // Build each frame once, outside the lock; then send full frames or deltas.
            let frames: Vec<Frame> = sessions.iter().map(|s| s.frame()).collect();
            let mut retry = Vec::new();
            {
                let mut inner = self.inner.lock().unwrap();
                for frame in &frames {
                    for client in inner.clients.values_mut().filter(|c| c.subs.contains(&frame.id)) {
                        let delta = (client.version >= 3)
                            .then(|| client.last.get(&frame.id).and_then(|old| FrameDelta::between(old, frame).map(|d| (d, old))))
                            .flatten();
                        let msg = match delta {
                            Some((d, old)) if d.is_noop(old) => continue,
                            Some((d, _)) => ServerMsg::FrameDelta(Box::new(d)),
                            None => ServerMsg::Frame(Box::new(frame.clone())),
                        };
                        match client.tx.try_send(msg) {
                            Ok(()) => {
                                client.last.insert(frame.id, frame.clone());
                            }
                            Err(TrySendError::Full(_)) => {
                                // Client is behind: forget the base and send a full frame next time.
                                client.last.remove(&frame.id);
                                retry.push(frame.id);
                            }
                            Err(TrySendError::Disconnected(_)) => {}
                        }
                    }
                }
            }
            for id in retry {
                self.mark_dirty(id);
            }
            thread::sleep(Duration::from_millis(7));
        }
    }

    fn housekeeping(self: Arc<Self>) {
        let mut last_scrollback = Instant::now();
        let mut tick: u64 = 0;
        loop {
            thread::sleep(Duration::from_secs(1));
            tick += 1;
            let sessions: Vec<Arc<Session>> = self.inner.lock().unwrap().sessions.values().cloned().collect();
            if tick % 2 == 0 {
                for s in &sessions {
                    if s.poll_process() {
                        self.broadcast(ServerMsg::SessionUpdated(s.info()));
                    }
                }
                self.track_claude(&sessions);
                self.hibernate_idle(&sessions);
                // Edits by agents or by hand apply without a window asking for it.
                if config_mtime() != *self.config_mtime.lock().unwrap() {
                    self.reload_config();
                }
            }
            // A chat that fell asleep just before its terminal was opened wakes up now.
            for s in &sessions {
                if s.watched.load(Ordering::Relaxed) && s.meta.lock().unwrap().claude_hibernated {
                    self.wake_claude(s);
                }
            }
            // Claude's state changes quickly (working / done / asking); check it every second.
            for s in &sessions {
                let running = s.meta.lock().unwrap().claude_running;
                if !running {
                    continue;
                }
                let state = s.claude_state();
                let changed = {
                    let mut m = s.meta.lock().unwrap();
                    let changed = m.claude_state != state;
                    if state != session::CLAUDE_READY {
                        m.claude_idle_since = None;
                    } else if m.claude_idle_since.is_none() {
                        m.claude_idle_since = Some(Instant::now());
                    }
                    m.claude_state = state;
                    changed
                };
                if changed {
                    if let Some(msg) = claude_msg(s) {
                        self.broadcast(msg);
                    }
                }
            }
            let ws_dirty = std::mem::take(&mut self.inner.lock().unwrap().ws_dirty);
            if ws_dirty || tick % 10 == 0 {
                self.save_state();
            }
            let interval = self.config.read().unwrap().persistence.interval_secs.max(2);
            if last_scrollback.elapsed() >= Duration::from_secs(interval) {
                last_scrollback = Instant::now();
                self.save_scrollback(&sessions, false);
            }
        }
    }

    fn save_state(&self) {
        let _guard = self.save_lock.lock().unwrap();
        if self.shutting_down.load(Ordering::SeqCst) {
            return;
        }
        let state = {
            let inner = self.inner.lock().unwrap();
            let sessions = inner
                .workspace
                .all_sessions()
                .iter()
                .filter_map(|id| inner.sessions.get(id))
                .map(|s| {
                    let meta = s.meta.lock().unwrap();
                    SavedSession {
                        id: s.id,
                        cwd: meta.cwd.clone(),
                        cols: meta.cols,
                        rows: meta.rows,
                        claude: meta.claude.clone(),
                        claude_title: meta.claude_title.clone(),
                        claude_running: meta.claude_running,
                        claude_flags: meta.claude_flags.clone(),
                        claude_hibernated: meta.claude_hibernated,
                    }
                })
                .collect();
            SavedState {
                version: 1,
                saved_at: now_secs(),
                workspace: inner.workspace.clone(),
                sessions,
                next_session: inner.next_session,
            }
        };
        match serde_json::to_vec(&state) {
            Ok(bytes) => {
                if let Err(err) = paths::write_private(&paths::state_file(), &bytes) {
                    log!("saving state failed: {err}");
                }
            }
            Err(err) => log!("serializing state failed: {err}"),
        }
    }

    fn save_scrollback(&self, sessions: &[Arc<Session>], all: bool) {
        let config = self.config.read().unwrap().persistence.clone();
        if !config.save_scrollback {
            return;
        }
        let _guard = self.save_lock.lock().unwrap();
        if self.shutting_down.load(Ordering::SeqCst) {
            return;
        }
        for s in sessions {
            if !all && !s.unsaved.swap(false, Ordering::Relaxed) {
                continue;
            }
            // Closed meanwhile: its file was deleted and must stay deleted.
            if !self.inner.lock().unwrap().sessions.contains_key(&s.id) {
                continue;
            }
            if let Some(bytes) = s.dump_ansi(config.scrollback_lines) {
                if let Err(err) = paths::write_private(&paths::scrollback_file(s.id), &bytes) {
                    log!("saving scrollback of {} failed: {err}", s.id);
                }
            }
        }
    }

    fn save_all(&self) {
        let sessions: Vec<Arc<Session>> = self.inner.lock().unwrap().sessions.values().cloned().collect();
        self.save_scrollback(&sessions, true);
        self.save_state();
    }

    /// Recreate the terminals of the previous daemon from disk.
    fn restore(self: &Arc<Self>) {
        let Ok(text) = fs::read_to_string(paths::state_file()) else { return };
        let saved: SavedState = match serde_json::from_str(&text) {
            Ok(saved) => saved,
            Err(err) => {
                log!("ignoring unreadable state file: {err}");
                return;
            }
        };
        let save_scrollback = self.config.read().unwrap().persistence.save_scrollback;
        let resume_claude = self.config.read().unwrap().persistence.resume_claude;
        self.inner.lock().unwrap().next_session = saved.next_session;
        let mut restored = 0;
        let mut seen_chats: HashSet<String> = HashSet::new();
        // Oldest first, so the terminal that started a chat keeps it.
        let mut sessions = saved.sessions;
        sessions.sort_by_key(|ss| ss.id);
        for ss in sessions {
            let mut bytes = if save_scrollback {
                fs::read(paths::scrollback_file(ss.id)).unwrap_or_default()
            } else {
                Vec::new()
            };
            bytes.extend_from_slice(b"\x1b[0m\r\n");
            match self.spawn_session(ss.id, Some(ss.cwd), ss.cols, ss.rows, Some(bytes)) {
                Ok(session) => {
                    restored += 1;
                    // A chat belongs to one terminal; older versions could record it twice.
                    if !ss.claude.is_empty() && seen_chats.insert(ss.claude.clone()) {
                        let mut m = session.meta.lock().unwrap();
                        m.claude = ss.claude.clone();
                        m.claude_title = ss.claude_title.clone();
                        m.claude_flags = ss.claude_flags.clone();
                        // A chat that was running comes back asleep and wakes when viewed,
                        // so a restart does not launch dozens of Claude processes at once.
                        m.claude_hibernated = ss.claude_hibernated || (resume_claude && ss.claude_running);
                    }
                }
                Err(err) => log!("restoring session {} failed: {err}", ss.id),
            }
            let mut inner = self.inner.lock().unwrap();
            inner.next_session = inner.next_session.max(ss.id);
        }
        let mut inner = self.inner.lock().unwrap();
        let live: HashSet<SessionId> = inner.sessions.keys().copied().collect();
        let mut ws = saved.workspace;
        ws.sanitize(&live);
        inner.workspace = ws;
        log!("restored {restored} sessions");
    }
}

fn claude_msg(s: &Session) -> Option<ServerMsg> {
    let m = s.meta.lock().unwrap();
    (!m.claude.is_empty() || m.claude_running || m.claude_hibernated).then(|| ServerMsg::Claude {
        id: s.id,
        session: m.claude.clone(),
        title: m.claude_title.clone(),
        running: m.claude_running,
        state: if m.claude_hibernated {
            session::CLAUDE_ASLEEP
        } else if m.claude_running {
            m.claude_state
        } else {
            session::CLAUDE_UNKNOWN
        },
    })
}

fn writer(stream: UnixStream, rx: Receiver<ServerMsg>) {
    let mut w = BufWriter::with_capacity(256 * 1024, stream);
    while let Ok(msg) = rx.recv() {
        if write_msg(&mut w, &msg).is_err() {
            return;
        }
        while let Ok(msg) = rx.try_recv() {
            if write_msg(&mut w, &msg).is_err() {
                return;
            }
        }
        if w.flush().is_err() {
            return;
        }
    }
}

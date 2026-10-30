//! Command line control of a running sennya, designed for scripts and AI agents:
//! every command prints JSON (or plain text for `read`).

use std::io::{BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::json;

use crate::config::{Config, DEFAULT_CONFIG};
use crate::client::connect_or_spawn;
use crate::model::{SessionId, Workspace};
use crate::paths;
use crate::protocol::{read_msg, write_msg, ClientMsg, ServerMsg, SessionInfo, SpawnSpec, PROTOCOL_VERSION};

pub const HELP: &str = "sennya — a fast terminal whose sessions survive restarts

USAGE:
    sennya                              open the window (starts the session daemon if needed)

  Control (JSON output; ID may be `current` inside a sennya terminal):
    sennya list                         pages and terminals
    sennya new [--near ID] [--open tab|right|down|stack|page] [--cwd DIR] [--page NAME] [--title T] [-- CMD...]
                                        open a terminal (default cwd: here) and run CMD in it
    sennya claude [--near ID] [--open WHERE] [--skip-permissions] [--no-submit] [--model M] PROMPT...
                                        start Claude Code on a task in a new terminal
    sennya run PROFILE [--near ID] [--var Label=value]...
                                        run a [[profile]] from the config
    sennya send ID TEXT... [--no-enter] type text into a terminal (Enter is pressed by default)
    sennya read ID [--lines N]          last N lines of a terminal as text (default 50)
    sennya focus ID | hide ID | show ID | close ID
                                        show, hide from the grid (keeps running), close
    sennya layout grid|columns|rows|main-stack|manual
                                        how the current page tiles
    sennya profiles                     profiles defined in the config
    sennya history [--limit N]          Claude Code chats, newest first
    sennya resume CLAUDE_SESSION_ID     reopen a Claude Code chat in a new terminal

  Servers (terminals that survive network drops):
    sennya remote install HOST [--install-rust]
                                        put a sennya daemon on HOST (any `ssh HOST` target)
    sennya remote HOST                  open a window attached to HOST's daemon

  Daemon and config:
    sennya install-skill                teach Claude Code to use sennya (~/.claude/skills/sennya)
    sennya config path | example        where the config lives / the documented default config
    sennya restart-daemon               restart the daemon (terminals come back with their history)
    sennya kill                         stop the daemon and every terminal
    sennya daemon                       run the session daemon in the foreground

Config: ~/.config/sennya/config.toml (reloaded automatically).
Guide for AI agents: sennya help agents";

pub const AGENTS_GUIDE: &str = include_str!("../AGENTS.md");
pub const CLAUDE_SKILL: &str = include_str!("../skills/sennya/SKILL.md");

struct Conn {
    w: UnixStream,
    r: BufReader<UnixStream>,
    workspace: Workspace,
    sessions: Vec<SessionInfo>,
    daemon_pid: u32,
}

impl Conn {
    fn open(spawn: bool) -> Result<Conn> {
        let stream = if spawn {
            connect_or_spawn().context("cannot start the sennya daemon")?
        } else {
            UnixStream::connect(paths::socket_path()).map_err(|_| anyhow!("sennya is not running (start it with `sennya`)"))?
        };
        stream.set_read_timeout(Some(Duration::from_secs(15)))?;
        let mut conn = Conn {
            w: stream.try_clone()?,
            r: BufReader::new(stream),
            workspace: Workspace::new(crate::model::ViewMode::Tabs),
            sessions: Vec::new(),
            daemon_pid: 0,
        };
        conn.send(ClientMsg::Hello { version: PROTOCOL_VERSION })?;
        let mut version = 0;
        loop {
            match conn.recv()? {
                ServerMsg::Hello { version: v, pid } => {
                    version = v;
                    conn.daemon_pid = pid;
                    if v < crate::protocol::MIN_DAEMON_VERSION {
                        break;
                    }
                }
                ServerMsg::State { workspace, sessions } => {
                    conn.workspace = workspace;
                    conn.sessions = sessions;
                    break;
                }
                _ => {}
            }
        }
        if version < crate::protocol::MIN_DAEMON_VERSION {
            bail!("the running daemon is older than this sennya; run `sennya restart-daemon` (terminals keep their folders and history)");
        }
        Ok(conn)
    }

    fn send(&mut self, msg: ClientMsg) -> Result<()> {
        write_msg(&mut self.w, &msg)?;
        self.w.flush()?;
        Ok(())
    }

    fn recv(&mut self) -> Result<ServerMsg> {
        read_msg(&mut self.r).context("no answer from the sennya daemon")
    }

    fn wait<T>(&mut self, mut f: impl FnMut(ServerMsg) -> Option<T>) -> Result<T> {
        loop {
            if let Some(v) = f(self.recv()?) {
                return Ok(v);
            }
        }
    }

    fn title(&self, info: &SessionInfo) -> String {
        if let Some(name) = self.workspace.names.get(&info.id) {
            return name.clone();
        }
        if !info.osc_title.trim().is_empty() {
            return info.osc_title.trim().to_string();
        }
        if !info.process.is_empty() {
            return info.process.clone();
        }
        info.cwd.rsplit('/').next().unwrap_or_default().to_string()
    }

    fn ack(&mut self, req: u64) -> Result<Option<SessionId>> {
        self.wait(|m| match m {
            ServerMsg::Ack { req: r, ok, message, session } if r == req => Some(if ok { Ok(session) } else { Err(anyhow!(message)) }),
            _ => None,
        })?
    }
}

fn session_id(arg: Option<&String>) -> Result<SessionId> {
    let arg = arg.ok_or_else(|| anyhow!("missing terminal id (a number from `sennya list`, or `current`)"))?;
    if arg == "current" {
        return std::env::var("SENNYA_SESSION")
            .ok()
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| anyhow!("`current` only works inside a sennya terminal"));
    }
    arg.parse().map_err(|_| anyhow!("not a terminal id: {arg}"))
}

/// Index where the command's own arguments start (`--`), so options after it are left alone.
fn options_end(args: &[String]) -> usize {
    args.iter().position(|a| a == "--").unwrap_or(args.len())
}

fn flag(args: &mut Vec<String>, name: &str) -> Option<String> {
    let i = args[..options_end(args)].iter().position(|a| a == name)?;
    if i + 1 >= options_end(args) {
        return None;
    }
    let value = args.remove(i + 1);
    args.remove(i);
    Some(value)
}

fn switch(args: &mut Vec<String>, name: &str) -> bool {
    match args[..options_end(args)].iter().position(|a| a == name) {
        Some(i) => {
            args.remove(i);
            true
        }
        None => false,
    }
}

fn print(v: serde_json::Value) {
    println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
}

/// Returns false when `cmd` is not a CLI command.
pub fn run(cmd: &str, mut args: Vec<String>) -> Result<bool> {
    match cmd {
        "list" | "ls" => {
            let c = Conn::open(false)?;
            let focused = c.workspace.focused();
            let pages: Vec<_> = c
                .workspace
                .pages
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    let terminals: Vec<_> = p
                        .sessions()
                        .into_iter()
                        .filter_map(|id| c.sessions.iter().find(|s| s.id == id))
                        .map(|s| {
                            json!({
                                "id": s.id,
                                "title": c.title(s),
                                "cwd": s.cwd,
                                "process": s.process,
                                "focused": Some(s.id) == focused,
                                "hidden": p.hidden.contains(&s.id),
                            })
                        })
                        .collect();
                    let layout = match p.arrange {
                        crate::model::Arrange::Grid => "grid",
                        crate::model::Arrange::Columns => "columns",
                        crate::model::Arrange::Rows => "rows",
                        crate::model::Arrange::MainStack => "main-stack",
                        crate::model::Arrange::Manual => "manual",
                    };
                    json!({ "page": i + 1, "name": p.name, "active": i == c.workspace.active_page, "layout": layout, "terminals": terminals })
                })
                .collect();
            print(json!({ "mode": format!("{:?}", c.workspace.mode).to_lowercase(), "pages": pages }));
        }
        "new" | "claude" => {
            let near = flag(&mut args, "--near").map(|n| session_id(Some(&n))).transpose()?;
            let here = std::env::current_dir().ok();
            let cwd = match flag(&mut args, "--cwd") {
                // The daemon runs in $HOME; resolve relative paths against this command's folder.
                Some(dir) if !dir.starts_with('/') && !dir.starts_with('~') => here
                    .map(|h| h.join(&dir))
                    .map(|p| std::fs::canonicalize(&p).unwrap_or(p).to_string_lossy().into_owned()),
                Some(dir) => Some(dir),
                None => here.map(|d| d.to_string_lossy().into_owned()),
            };
            let default_open = if near.is_some() { "right" } else { "tab" };
            let open = flag(&mut args, "--open").unwrap_or_else(|| default_open.into());
            if !["tab", "right", "down", "stack", "page"].contains(&open.as_str()) {
                bail!("--open must be tab, right, down, stack or page");
            }
            let page = flag(&mut args, "--page");
            let mut title = flag(&mut args, "--title");
            let mut spec = SpawnSpec { cwd, open, near, page, ..Default::default() };
            if cmd == "claude" {
                let skip = switch(&mut args, "--skip-permissions");
                let no_submit = switch(&mut args, "--no-submit");
                let model = flag(&mut args, "--model");
                if args.first().is_some_and(|a| a == "--") {
                    args.remove(0);
                }
                let prompt = args.join(" ");
                let mut line = String::from("claude");
                if skip {
                    line.push_str(" --dangerously-skip-permissions");
                }
                if let Some(m) = model {
                    line.push_str(&format!(" --model {}", crate::template::shell_quote(&m)));
                }
                if !prompt.is_empty() && !no_submit {
                    // `--` so a prompt starting with "-" (a list, say) is not read as an option.
                    line.push_str(if prompt.starts_with('-') { " -- " } else { " " });
                    line.push_str(&crate::template::shell_quote(&prompt));
                }
                spec.run = vec![line];
                if no_submit && !prompt.is_empty() {
                    // Typed once Claude Code's prompt box is up.
                    spec.prefill = Some(prompt.clone());
                    spec.prefill_after = Some("@claude-ready".into());
                }
                if title.is_none() && !prompt.is_empty() {
                    let short: String = prompt.chars().take(32).collect();
                    title = Some(format!("✳ {short}"));
                }
            } else {
                if args.first().is_some_and(|a| a == "--") {
                    args.remove(0);
                }
                spec.run = if args.is_empty() { Vec::new() } else { vec![args.join(" ")] };
            }
            spec.title = title;
            let mut c = Conn::open(true)?;
            c.send(ClientMsg::Spawn { req: 1, spec })?;
            let id = c.ack(1)?;
            print(json!({ "id": id }));
        }
        "run" => {
            let near = flag(&mut args, "--near").map(|n| session_id(Some(&n))).transpose()?;
            let name = if args.is_empty() { bail!("usage: sennya run PROFILE [--var Label=value]") } else { args.remove(0) };
            let mut vars = Vec::new();
            while let Some(v) = flag(&mut args, "--var") {
                let (k, val) = v.split_once('=').ok_or_else(|| anyhow!("--var expects Label=value"))?;
                vars.push((format!("prompt:{k}"), val.to_string()));
            }
            if let Ok(dir) = std::env::current_dir() {
                vars.push(("cwd".into(), dir.to_string_lossy().into_owned()));
            }
            let mut c = Conn::open(true)?;
            c.send(ClientMsg::RunProfile { req: 1, name, vars, near })?;
            let id = c.ack(1)?;
            print(json!({ "id": id }));
        }
        "send" => {
            let enter = !switch(&mut args, "--no-enter");
            let id = session_id(args.first())?;
            let mut text = args[1..].join(" ");
            if enter {
                text.push('\r');
            }
            let mut c = Conn::open(false)?;
            if !c.sessions.iter().any(|s| s.id == id) {
                bail!("no terminal {id} (see `sennya list`)");
            }
            c.send(ClientMsg::Input(id, text.into_bytes()))?;
            print(json!({ "ok": true }));
        }
        "read" => {
            let lines = flag(&mut args, "--lines").and_then(|n| n.parse().ok()).unwrap_or(50);
            let id = session_id(args.first())?;
            let mut c = Conn::open(false)?;
            if !c.sessions.iter().any(|s| s.id == id) {
                bail!("no terminal {id} (see `sennya list`)");
            }
            c.send(ClientMsg::ReadText { req: 1, id, lines })?;
            let text = c.wait(|m| match m {
                ServerMsg::Text { req: 1, text } => Some(text),
                _ => None,
            })?;
            println!("{text}");
        }
        "focus" | "hide" | "show" | "close" => {
            let id = session_id(args.first())?;
            let mut c = Conn::open(false)?;
            if !c.sessions.iter().any(|s| s.id == id) {
                bail!("no terminal {id} (see `sennya list`)");
            }
            c.send(match cmd {
                "focus" => ClientMsg::Focus(id),
                "hide" => ClientMsg::SetHidden { id, hidden: true },
                "show" => ClientMsg::SetHidden { id, hidden: false },
                _ => ClientMsg::CloseSession(id),
            })?;
            print(json!({ "ok": true }));
        }
        "layout" => {
            use crate::model::Arrange;
            let arrange = match args.first().map(String::as_str) {
                Some("grid") => Arrange::Grid,
                Some("columns") => Arrange::Columns,
                Some("rows") => Arrange::Rows,
                Some("main-stack" | "stack") => Arrange::MainStack,
                Some("manual") => Arrange::Manual,
                _ => bail!("usage: sennya layout grid|columns|rows|main-stack|manual [--page N]"),
            };
            let page = flag(&mut args, "--page").and_then(|p| p.parse::<u32>().ok()).map(|p| p.saturating_sub(1));
            let mut c = Conn::open(false)?;
            c.send(ClientMsg::Arrange { page, arrange })?;
            print(json!({ "ok": true }));
        }
        "remote" => match args.first().map(String::as_str) {
            Some("install") => {
                let host = args.get(1).ok_or_else(|| anyhow!("usage: sennya remote install HOST [--install-rust]"))?.clone();
                let install_rust = args.iter().any(|a| a == "--install-rust");
                remote_install(&host, install_rust)?;
            }
            Some(host) => {
                std::process::Command::new(std::env::current_exe()?).args(["--remote", host]).spawn()?;
                print(json!({ "ok": true, "host": host }));
            }
            None => bail!("usage: sennya remote HOST | sennya remote install HOST"),
        },
        "install-skill" => {
            let dir = std::env::var_os("CLAUDE_CONFIG_DIR")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".claude"))
                .join("skills")
                .join("sennya");
            std::fs::create_dir_all(&dir)?;
            let path = dir.join("SKILL.md");
            std::fs::write(&path, CLAUDE_SKILL)?;
            print(json!({ "ok": true, "path": path }));
        }
        "help" => match args.first().map(String::as_str) {
            Some("agents") => print!("{AGENTS_GUIDE}"),
            _ => println!("{HELP}"),
        },
        "profiles" => {
            let (config, err) = Config::load();
            if let Some(err) = err {
                bail!(err);
            }
            let profiles: Vec<_> = config
                .profiles
                .iter()
                .map(|p| {
                    json!({
                        "name": p.name,
                        "description": p.description,
                        "key": p.key,
                        "open": p.open,
                        "prompts": crate::template::prompts(p.texts()),
                    })
                })
                .collect();
            print(json!(profiles));
        }
        "history" => {
            let limit = flag(&mut args, "--limit").and_then(|n| n.parse().ok()).unwrap_or(30);
            let mut c = Conn::open(true)?;
            c.send(ClientMsg::ClaudeHistory { req: 1, limit })?;
            let sessions = c.wait(|m| match m {
                ServerMsg::ClaudeHistory { req: 1, sessions } => Some(sessions),
                _ => None,
            })?;
            print(serde_json::to_value(sessions)?);
        }
        "resume" => {
            let id = args.first().ok_or_else(|| anyhow!("usage: sennya resume CLAUDE_SESSION_ID"))?.clone();
            let mut c = Conn::open(true)?;
            c.send(ClientMsg::ClaudeHistory { req: 1, limit: 2000 })?;
            let sessions = c.wait(|m| match m {
                ServerMsg::ClaudeHistory { req: 1, sessions } => Some(sessions),
                _ => None,
            })?;
            let s = sessions.into_iter().find(|s| s.id == id || s.id.starts_with(&id)).ok_or_else(|| anyhow!("no Claude Code chat {id}"))?;
            let spec = SpawnSpec {
                cwd: Some(s.cwd.clone()),
                open: "tab".into(),
                title: Some(s.title.chars().take(40).collect()),
                run: vec![format!("claude --resume {}", s.id)],
                ..Default::default()
            };
            c.send(ClientMsg::Spawn { req: 2, spec })?;
            let terminal = c.ack(2)?;
            print(json!({ "id": terminal, "claude_session": s.id, "title": s.title }));
        }
        "config" => match args.first().map(String::as_str) {
            Some("path") | None => println!("{}", paths::config_file().display()),
            Some("example") => print!("{DEFAULT_CONFIG}"),
            Some(other) => bail!("unknown: sennya config {other} (use path or example)"),
        },
        "restart-daemon" => {
            let pid = match Conn::open(false) {
                Ok(c) => c.daemon_pid,
                Err(_) => {
                    // An older daemon still answers Hello; read its pid without the version check.
                    let s = UnixStream::connect(paths::socket_path()).map_err(|_| anyhow!("sennya is not running"))?;
                    s.set_read_timeout(Some(Duration::from_secs(5)))?;
                    let mut r = BufReader::new(s.try_clone()?);
                    match read_msg::<_, ServerMsg>(&mut r)? {
                        ServerMsg::Hello { pid, .. } => pid,
                        _ => bail!("unexpected answer from the daemon"),
                    }
                }
            };
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }
            let mut gone = false;
            for _ in 0..300 {
                if unsafe { libc::kill(pid as i32, 0) } != 0 {
                    gone = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            if !gone {
                bail!("the daemon (pid {pid}) is still saving after 15 s; try again in a moment");
            }
            let c = Conn::open(true)?;
            print(json!({ "ok": true, "old_pid": pid, "new_pid": c.daemon_pid, "terminals": c.sessions.len() }));
        }
        _ => return Ok(false),
    }
    Ok(true)
}

fn ssh_program() -> String {
    std::env::var("SENNYA_SSH").unwrap_or_else(|_| "ssh".into())
}

fn ssh_command(host: &str, script: &str) -> std::process::Command {
    let mut cmd = std::process::Command::new(ssh_program());
    cmd.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10", host, script]);
    cmd
}

fn ssh_output(host: &str, script: &str) -> Result<String> {
    let out = ssh_command(host, script).output().with_context(|| format!("cannot run ssh {host}"))?;
    if !out.status.success() {
        bail!("ssh {host}: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Put a daemon-only sennya on a server at `~/.sennya/bin/sennya`.
fn remote_install(host: &str, install_rust: bool) -> Result<()> {
    use std::process::Stdio;
    eprintln!("→ checking {host}");
    let uname = ssh_output(host, "uname -sm")?;
    let (os, arch) = uname.split_once(' ').ok_or_else(|| anyhow!("unexpected `uname -sm` from {host}: {uname}"))?;
    let arch = match arch {
        "arm64" | "aarch64" => "aarch64",
        "x86_64" | "amd64" => "x86_64",
        other => other,
    };
    let same_machine_kind = os == "Darwin" && cfg!(target_os = "macos") && arch == std::env::consts::ARCH;
    if same_machine_kind && std::env::var_os("SENNYA_REMOTE_FORCE_BUILD").is_none() {
        eprintln!("→ {host} is a Mac ({arch}): copying this sennya");
        let exe = std::fs::File::open(std::env::current_exe()?)?;
        let status = ssh_command(host, "mkdir -p ~/.sennya/bin && cat > ~/.sennya/bin/sennya.new && chmod +x ~/.sennya/bin/sennya.new && mv ~/.sennya/bin/sennya.new ~/.sennya/bin/sennya")
            .stdin(exe)
            .status()?;
        if !status.success() {
            bail!("copying to {host} failed");
        }
    } else {
        eprintln!("→ {host} runs {os} on {arch}: building the daemon there from source");
        let cargo = "CARGO=$(command -v cargo || echo \"$HOME/.cargo/bin/cargo\")";
        if ssh_output(host, &format!("{cargo}; test -x \"$CARGO\" && echo yes || echo no"))? != "yes" {
            if !install_rust {
                bail!(
                    "Rust is not installed on {host}. Run `sennya remote install {host} --install-rust` \
                     to install it there with rustup (in your home folder, no sudo), or install it yourself."
                );
            }
            eprintln!("→ installing Rust on {host} (rustup, minimal profile)");
            let status = ssh_command(host, "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal")
                .status()?;
            if !status.success() {
                bail!("installing Rust on {host} failed");
            }
        }
        let source = env!("CARGO_MANIFEST_DIR");
        if !std::path::Path::new(source).join("Cargo.toml").exists() {
            bail!("the sennya source folder ({source}) is needed to build for {os}");
        }
        let mut tar = std::process::Command::new("tar")
            .args(["-C", source, "-czf", "-", "Cargo.toml", "Cargo.lock", "src", "skills", "AGENTS.md"])
            .env("COPYFILE_DISABLE", "1")
            .stdout(Stdio::piped())
            .spawn()?;
        let script = format!(
            "set -e; {cargo}; rm -rf ~/.sennya/src && mkdir -p ~/.sennya/src ~/.sennya/bin && tar -C ~/.sennya/src -xzf - \
             && cd ~/.sennya/src && \"$CARGO\" build --release --no-default-features --quiet \
             && cp target/release/sennya ~/.sennya/bin/sennya.new && mv ~/.sennya/bin/sennya.new ~/.sennya/bin/sennya"
        );
        let status = ssh_command(host, &script).stdin(tar.stdout.take().unwrap()).status()?;
        let _ = tar.wait();
        if !status.success() {
            bail!("building on {host} failed (see the output above)");
        }
    }
    // On PATH for agents on the server, and the skill for Claude Code there.
    ssh_output(host, "mkdir -p ~/.local/bin && ln -sf ~/.sennya/bin/sennya ~/.local/bin/sennya && ~/.sennya/bin/sennya install-skill >/dev/null")?;
    let version = ssh_output(host, "~/.sennya/bin/sennya --version")?;
    eprintln!("✓ {version} installed on {host}");
    print(json!({ "ok": true, "host": host, "version": version, "open": format!("sennya remote {host}") }));
    Ok(())
}

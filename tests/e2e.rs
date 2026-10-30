//! End-to-end: a real daemon, a real shell, the real socket protocol, and a
//! `kill -9` of the daemon followed by restoring the session from disk.

use std::io::{BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use sennya::model::{Dir, ViewMode, Workspace};
use sennya::protocol::{read_msg, write_msg, ClientMsg, Frame, SearchQuery, SelectCmd, SelectKind, ServerMsg};

struct Env {
    root: PathBuf,
    pids: std::cell::RefCell<Vec<u32>>,
}

impl Env {
    fn new() -> Env {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!("sennya-e2e-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("config/sennya")).unwrap();
        std::fs::write(
            root.join("config/sennya/config.toml"),
            "[shell]\nprogram = \"/bin/sh\"\n[persistence]\ninterval_secs = 2\n",
        )
        .unwrap();
        Env { root, pids: Default::default() }
    }

    fn data(&self) -> PathBuf {
        self.root.join("data")
    }

    fn daemon(&self) -> Child {
        let log = std::fs::OpenOptions::new().create(true).append(true).open(self.root.join("daemon.log")).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_sennya"))
            .arg("daemon")
            .env("SENNYA_DATA_DIR", self.data())
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("PS1", "$ ")
            .stdin(std::process::Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap();
        self.pids.borrow_mut().push(child.id());
        child
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        for pid in self.pids.borrow().iter() {
            unsafe {
                libc::kill(*pid as i32, libc::SIGKILL);
            }
        }
        if std::thread::panicking() {
            if let Ok(log) = std::fs::read_to_string(self.root.join("daemon.log")) {
                eprintln!("--- daemon log ---\n{log}");
            }
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

struct Client {
    w: UnixStream,
    r: BufReader<UnixStream>,
}

impl Client {
    fn connect(sock: &Path) -> Client {
        let start = Instant::now();
        loop {
            if let Ok(s) = UnixStream::connect(sock) {
                s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
                return Client { w: s.try_clone().unwrap(), r: BufReader::new(s) };
            }
            assert!(start.elapsed() < Duration::from_secs(5), "daemon socket never appeared");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn send(&mut self, msg: ClientMsg) {
        write_msg(&mut self.w, &msg).unwrap();
        self.w.flush().unwrap();
    }

    fn recv(&mut self) -> ServerMsg {
        read_msg(&mut self.r).expect("daemon message")
    }

    fn wait_for<T>(&mut self, what: &str, mut f: impl FnMut(&ServerMsg) -> Option<T>) -> T {
        let start = Instant::now();
        loop {
            assert!(start.elapsed() < Duration::from_secs(10), "timed out waiting for {what}");
            let msg = self.recv();
            if let Some(v) = f(&msg) {
                return v;
            }
        }
    }
}

fn screen_text(frame: &Frame) -> String {
    frame
        .lines
        .iter()
        .map(|l| l.runs.iter().map(|r| r.text.as_str()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn daemon_end_to_end() {
    let env = Env::new();
    let sock = env.data().join("sennya.sock");
    let mut daemon = env.daemon();
    let mut c = Client::connect(&sock);

    assert!(matches!(c.recv(), ServerMsg::Hello { .. }));
    let ws = c.wait_for("state", |m| match m {
        ServerMsg::State { workspace, sessions } => {
            assert!(sessions.is_empty());
            Some(workspace.clone())
        }
        _ => None,
    });
    assert_eq!(ws.all_sessions().len(), 0);

    // A shell starts and its output reaches us as frames.
    c.send(ClientMsg::CreateSession { req: 7, cwd: Some("/tmp".into()), cols: 80, rows: 24 });
    let id = c.wait_for("session", |m| match m {
        ServerMsg::SessionCreated { req: 7, info } => Some(info.id),
        _ => None,
    });
    c.send(ClientMsg::Subscribe(vec![id]));
    c.send(ClientMsg::Input(id, b"echo hello-$((40+2))-sennya\r".to_vec()));
    let frame = c.wait_for("echo output", |m| match m {
        ServerMsg::Frame(f) if f.id == id && screen_text(f).contains("hello-42-sennya") => Some(f.clone()),
        _ => None,
    });
    assert_eq!((frame.cols, frame.rows), (80, 24));

    // Resizing reflows the grid.
    c.send(ClientMsg::Resize { id, cols: 100, rows: 30, cell_w: 16, cell_h: 32 });
    c.wait_for("resized frame", |m| match m {
        ServerMsg::Frame(f) if f.id == id && f.cols == 100 && f.rows == 30 => Some(()),
        _ => None,
    });

    // Selecting a word and copying it.
    let (row, col) = frame
        .lines
        .iter()
        .enumerate()
        .find_map(|(r, l)| {
            let text: String = l.runs.iter().map(|x| x.text.as_str()).collect();
            (text.starts_with("hello-42")).then_some((r as u16, 2u16))
        })
        .expect("output line");
    c.send(ClientMsg::Select(id, SelectCmd::Start { row, col, right: false, kind: SelectKind::Line }));
    c.send(ClientMsg::Copy { req: 9, id });
    let copied = c.wait_for("copy", |m| match m {
        ServerMsg::CopyText { req: 9, text } => Some(text.clone()),
        _ => None,
    });
    assert_eq!(copied.trim(), "hello-42-sennya");

    // Searching across terminals.
    c.send(ClientMsg::Search {
        req: 11,
        query: SearchQuery { text: "42-sen".into(), regex: false, case_sensitive: false },
    });
    let hits = c.wait_for("search", |m| match m {
        ServerMsg::SearchResults { req: 11, hits } => Some(hits.clone()),
        _ => None,
    });
    assert!(hits.iter().any(|h| h.session == id && h.snippet.contains("hello-42-sennya")), "{hits:?}");

    // The GUI places the session in the workspace.
    let mut ws = Workspace::new(ViewMode::Grid);
    ws.add_session(id, 0, None, Dir::Horizontal);
    ws.pages[0].name = "Work".into();
    c.send(ClientMsg::SetWorkspace(ws));

    // Let a scrollback snapshot happen, then kill the daemon without warning.
    std::thread::sleep(Duration::from_millis(3500));
    unsafe {
        libc::kill(daemon.id() as i32, libc::SIGKILL);
    }
    daemon.wait().unwrap();
    drop(c);
    let _ = std::fs::remove_file(&sock);

    // A new daemon brings the terminal back with its history and layout.
    let mut daemon = env.daemon();
    let mut c = Client::connect(&sock);
    let (ws, sessions) = c.wait_for("restored state", |m| match m {
        ServerMsg::State { workspace, sessions } => Some((workspace.clone(), sessions.clone())),
        _ => None,
    });
    assert_eq!(ws.pages[0].name, "Work");
    assert_eq!(ws.all_sessions(), vec![id]);
    assert!(sessions[0].restored);
    assert_eq!(std::fs::canonicalize(&sessions[0].cwd).unwrap(), std::fs::canonicalize("/tmp").unwrap());
    c.send(ClientMsg::Subscribe(vec![id]));
    c.wait_for("restored scrollback", |m| match m {
        ServerMsg::Frame(f) if f.id == id => {
            let text = screen_text(f);
            text.contains("hello-42-sennya").then_some(())
        }
        _ => None,
    });
    // The restored terminal has a live shell.
    c.send(ClientMsg::Input(id, b"echo alive-$((6*7))\r".to_vec()));
    c.wait_for("live shell", |m| match m {
        ServerMsg::Frame(f) if f.id == id && screen_text(f).contains("alive-42") => Some(()),
        _ => None,
    });

    // Closing a session reports it as gone; shutdown removes all state.
    c.send(ClientMsg::CloseSession(id));
    c.wait_for("exit", |m| matches!(m, ServerMsg::SessionExited(x) if *x == id).then_some(()));
    c.send(ClientMsg::Shutdown);
    let status = daemon.wait().unwrap();
    assert!(status.success());
    assert!(!env.data().join("state.json").exists());
}

/// `cargo test --release --test e2e -- --ignored --nocapture input_latency`
/// Time from sending a key to receiving the frame that shows its echo.
#[test]
#[ignore]
fn input_latency() {
    let env = Env::new();
    let sock = env.data().join("sennya.sock");
    let _daemon = env.daemon();
    let mut c = Client::connect(&sock);
    c.send(ClientMsg::CreateSession { req: 1, cwd: Some("/tmp".into()), cols: 120, rows: 40 });
    let id = c.wait_for("session", |m| match m {
        ServerMsg::SessionCreated { req: 1, info } => Some(info.id),
        _ => None,
    });
    c.send(ClientMsg::Subscribe(vec![id]));
    std::thread::sleep(Duration::from_millis(300));
    c.r.get_ref().set_nonblocking(true).unwrap();
    while read_msg::<_, ServerMsg>(&mut c.r).is_ok() {}
    c.r.get_ref().set_nonblocking(false).unwrap();
    let mut samples = Vec::new();
    for i in 0..60 {
        let ch = (b'a' + (i % 26) as u8) as char;
        let typed: String = (0..=i).map(|k| (b'a' + (k % 26) as u8) as char).collect();
        let t = Instant::now();
        c.send(ClientMsg::Input(id, vec![ch as u8]));
        c.wait_for("echo", |m| match m {
            ServerMsg::Frame(f) if f.id == id && screen_text(f).contains(&typed) => Some(()),
            _ => None,
        });
        samples.push(t.elapsed());
        std::thread::sleep(Duration::from_millis(30));
    }
    samples.sort();
    let avg = samples.iter().sum::<Duration>() / samples.len() as u32;
    println!(
        "key -> echo frame: avg {avg:?}  p50 {:?}  p95 {:?}  max {:?}",
        samples[samples.len() / 2],
        samples[samples.len() * 95 / 100],
        samples[samples.len() - 1]
    );
    c.send(ClientMsg::Shutdown);
}

/// `cargo test --test e2e -- --ignored --nocapture dump_prompt_frame`
#[test]
#[ignore]
fn dump_prompt_frame() {
    let env = Env::new();
    std::fs::write(env.root.join("config/sennya/config.toml"), "").unwrap();
    let sock = env.data().join("sennya.sock");
    let _daemon = env.daemon();
    let mut c = Client::connect(&sock);
    let start_cols: u16 = std::env::var("START_COLS").ok().and_then(|v| v.parse().ok()).unwrap_or(100);
    c.send(ClientMsg::CreateSession { req: 1, cwd: None, cols: start_cols, rows: 10 });
    let id = c.wait_for("session", |m| match m {
        ServerMsg::SessionCreated { req: 1, info } => Some(info.id),
        _ => None,
    });
    c.send(ClientMsg::Subscribe(vec![id]));
    std::thread::sleep(Duration::from_millis(2500));
    c.send(ClientMsg::Resize { id, cols: 100, rows: 10, cell_w: 16, cell_h: 32 });
    std::thread::sleep(Duration::from_millis(500));
    c.r.get_ref().set_nonblocking(true).unwrap();
    while read_msg::<_, ServerMsg>(&mut c.r).is_ok() {}
    c.r.get_ref().set_nonblocking(false).unwrap();
    c.send(ClientMsg::Scroll(id, sennya::protocol::ScrollCmd::Bottom));
    let frame = c.wait_for("frame", |m| match m {
        ServerMsg::Frame(f) if f.id == id && screen_text(f).contains('%') || matches!(m, ServerMsg::Frame(f) if f.id == id && screen_text(f).contains('$')) => match m {
            ServerMsg::Frame(f) => Some(f.clone()),
            _ => None,
        },
        _ => None,
    });
    for (r, line) in frame.lines.iter().enumerate() {
        for run in &line.runs {
            println!("row {r} col {} width {} {:?} flags {}", run.col, run.width, run.text, run.flags);
        }
    }
    println!("cursor {:?}", frame.cursor);
    c.send(ClientMsg::Shutdown);
}

/// The agent-facing CLI: new / list / send / read / run a profile with prompts and waits.
#[test]
fn cli_and_profiles() {
    let env = Env::new();
    std::fs::write(
        env.root.join("config/sennya/config.toml"),
        r#"
[shell]
program = "/bin/sh"

[[profile]]
name = "task"
open = "page"
page = "task {prompt:Name}"
title = "t-{short}"
cwd = "/tmp"
steps = [
  "PS1='remote> '",
  { wait_for = ">", run = "mkdir -p {home}/.sennya-e2e/{prompt:Name}-{timestamp} && cd $_ && pwd" },
]
"#,
    )
    .unwrap();
    let _daemon = env.daemon();
    let sock = env.data().join("sennya.sock");
    let _ = Client::connect(&sock);
    let cli = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_sennya"))
            .args(args)
            .env("SENNYA_DATA_DIR", env.data())
            .env("XDG_CONFIG_HOME", env.root.join("config"))
            .current_dir("/tmp")
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    };
    let v: serde_json::Value = serde_json::from_str(&cli(&["new", "--title", "build", "--", "echo", "from-cli-$((20+3))"])).unwrap();
    let id = v["id"].as_u64().unwrap().to_string();
    let wait_text = |id: &str, needle: &str| {
        let start = Instant::now();
        loop {
            let text = cli(&["read", id, "--lines", "20"]);
            if text.contains(needle) {
                return text;
            }
            assert!(start.elapsed() < Duration::from_secs(10), "{needle:?} never appeared in:\n{text}");
            std::thread::sleep(Duration::from_millis(100));
        }
    };
    wait_text(&id, "from-cli-23");
    cli(&["send", &id, "echo sent-$((1+1))"]);
    wait_text(&id, "sent-2");

    let list: serde_json::Value = serde_json::from_str(&cli(&["list"])).unwrap();
    assert_eq!(list["pages"][0]["terminals"][0]["title"], "build");

    let profiles: serde_json::Value = serde_json::from_str(&cli(&["profiles"])).unwrap();
    assert_eq!(profiles[0]["prompts"][0], "Name");

    let v: serde_json::Value = serde_json::from_str(&cli(&["run", "task", "--var", "Name=login"])).unwrap();
    let pid = v["id"].as_u64().unwrap().to_string();
    let text = wait_text(&pid, ".sennya-e2e/login-20");
    assert!(text.contains("remote>"), "{text}");
    let list: serde_json::Value = serde_json::from_str(&cli(&["list"])).unwrap();
    assert_eq!(list["pages"][1]["name"], "task login");
    assert!(list["pages"][1]["terminals"][0]["title"].as_str().unwrap().starts_with("t-"));

    // An agent opens tasks beside its own terminal: main + stack.
    cli(&["focus", &id]);
    let a: serde_json::Value = serde_json::from_str(&cli(&["new", "--near", &id, "--open", "stack", "--", "echo", "task-a"])).unwrap();
    let b: serde_json::Value = serde_json::from_str(&cli(&["new", "--near", &id, "--open", "stack", "--", "echo", "task-b"])).unwrap();
    let list: serde_json::Value = serde_json::from_str(&cli(&["list"])).unwrap();
    let page = list["pages"].as_array().unwrap().iter().find(|p| p["terminals"].as_array().unwrap().iter().any(|t| t["id"].to_string() == id)).unwrap().clone();
    assert_eq!(page["layout"], "main-stack");
    let ids: Vec<String> = page["terminals"].as_array().unwrap().iter().map(|t| t["id"].to_string()).collect();
    assert_eq!(ids, vec![id.clone(), a["id"].to_string(), b["id"].to_string()]);
    wait_text(&b["id"].to_string(), "task-b");

    cli(&["hide", &a["id"].to_string()]);
    let list: serde_json::Value = serde_json::from_str(&cli(&["list"])).unwrap();
    let hidden: Vec<bool> = list["pages"][0]["terminals"].as_array().unwrap().iter().map(|t| t["hidden"].as_bool().unwrap()).collect();
    assert_eq!(hidden, vec![false, false, true]);
    cli(&["show", &a["id"].to_string()]);
    cli(&["layout", "columns"]);
    let list: serde_json::Value = serde_json::from_str(&cli(&["list"])).unwrap();
    assert_eq!(list["pages"][0]["layout"], "columns");
    cli(&["close", &b["id"].to_string()]);
    let guide = cli(&["help", "agents"]);
    assert!(guide.contains("sennya claude --near current"));

    let home = dirs::home_dir().unwrap();
    let _ = std::fs::remove_dir_all(home.join(".sennya-e2e"));
    cli(&["kill"]);
}

/// A window on another machine talks through `sennya proxy`; protocol 3 clients get line deltas.
#[test]
fn proxy_and_frame_deltas() {
    use std::process::Stdio;
    let env = Env::new();
    let _daemon = env.daemon();
    let _ = Client::connect(&env.data().join("sennya.sock"));
    let mut proxy = Command::new(env!("CARGO_BIN_EXE_sennya"))
        .arg("proxy")
        .env("SENNYA_DATA_DIR", env.data())
        .env("XDG_CONFIG_HOME", env.root.join("config"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut w = proxy.stdin.take().unwrap();
    let mut r = BufReader::new(proxy.stdout.take().unwrap());
    let mut send = |m: ClientMsg| {
        write_msg(&mut w, &m).unwrap();
        w.flush().unwrap();
    };
    send(ClientMsg::Hello { version: sennya::protocol::PROTOCOL_VERSION });
    send(ClientMsg::CreateSession { req: 1, cwd: Some("/tmp".into()), cols: 80, rows: 24 });
    let mut id = 0;
    let mut frame: Option<Frame> = None;
    let mut deltas = 0;
    let start = Instant::now();
    loop {
        assert!(start.elapsed() < Duration::from_secs(10), "timed out (deltas {deltas})");
        match read_msg::<_, ServerMsg>(&mut r).unwrap() {
            ServerMsg::SessionCreated { req: 1, info } => {
                id = info.id;
                send(ClientMsg::Subscribe(vec![id]));
                send(ClientMsg::Input(id, b"echo over-the-proxy-$((2*21))\r".to_vec()));
            }
            ServerMsg::Frame(f) if f.id == id => frame = Some(*f),
            ServerMsg::FrameDelta(d) if d.id == id => {
                deltas += 1;
                d.apply(frame.as_mut().expect("a delta needs a full frame first"));
            }
            _ => {}
        }
        if let Some(f) = &frame {
            if deltas > 0 && screen_text(f).contains("over-the-proxy-42") {
                break;
            }
        }
    }
    let _ = proxy.kill();
}


/// Idle, unseen Claude chats are closed to save memory and reopened (same flags, no
/// re-sent prompt) when their terminal is viewed again. Uses a stand-in `claude`.
#[test]
fn claude_hibernates_and_wakes() {
    let env = Env::new();
    let bin = env.root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let src = env.root.join("claude.c");
    std::fs::write(
        &src,
        r#"#include <stdio.h>
#include <string.h>
int main(int argc, char **argv) {
  printf("args:");
  for (int i = 1; i < argc; i++) printf(" %s", argv[i]);
  printf("\r\n\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\r\n");
  printf("\xe2\x9d\xaf \x1b[2mTry something\x1b[0m\r\n");
  printf("\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\r\n");
  fflush(stdout);
  char buf[256];
  while (fgets(buf, sizeof buf, stdin)) if (!strncmp(buf, "/exit", 5)) { printf("bye\r\n"); return 0; }
  return 0;
}
"#,
    )
    .unwrap();
    let status = Command::new("cc").arg(&src).arg("-o").arg(bin.join("claude")).status().unwrap();
    assert!(status.success(), "cc failed");

    // A transcript for the folder the stand-in runs in.
    let work = std::fs::canonicalize(&env.root).unwrap().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let cfg_dir = env.root.join("claude-config");
    let project = cfg_dir.join("projects").join(sennya::claude::encode_cwd(&work.to_string_lossy()));
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("1234abcd-0000-4000-8000-000000000001.jsonl"),
        format!("{{\"type\":\"user\",\"cwd\":\"{}\",\"message\":{{\"content\":\"hello\"}}}}\n", work.display()),
    )
    .unwrap();

    let log = std::fs::OpenOptions::new().create(true).append(true).open(env.root.join("daemon.log")).unwrap();
    let daemon = Command::new(env!("CARGO_BIN_EXE_sennya"))
        .arg("daemon")
        .env("SENNYA_DATA_DIR", env.data())
        .env("XDG_CONFIG_HOME", env.root.join("config"))
        .env("CLAUDE_CONFIG_DIR", &cfg_dir)
        .env("SENNYA_HIBERNATE_SECS", "2")
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("PS1", "$ ")
        .stdout(log.try_clone().unwrap())
        .stderr(log)
        .spawn()
        .unwrap();
    env.pids.borrow_mut().push(daemon.id());
    let mut c = Client::connect(&env.data().join("sennya.sock"));
    c.send(ClientMsg::CreateSession { req: 1, cwd: Some(work.to_string_lossy().into()), cols: 160, rows: 24 });
    let id = c.wait_for("session", |m| match m {
        ServerMsg::SessionCreated { req: 1, info } => Some(info.id),
        _ => None,
    });
    c.send(ClientMsg::Subscribe(vec![id]));
    std::thread::sleep(Duration::from_millis(300));
    c.send(ClientMsg::Input(id, b"claude --dangerously-skip-permissions 'first task'\r".to_vec()));
    c.wait_for("claude seen", |m| match m {
        ServerMsg::Claude { id: s, session, .. } if *s == id && session.starts_with("1234abcd") => Some(()),
        _ => None,
    });
    // Out of sight: it falls asleep after the idle time.
    c.send(ClientMsg::Subscribe(vec![]));
    c.wait_for("asleep", |m| match m {
        ServerMsg::Claude { id: s, state: 4, .. } if *s == id => Some(()),
        _ => None,
    });
    // Looking at it again resumes the same chat with the same options, without the prompt.
    c.send(ClientMsg::Subscribe(vec![id]));
    let text = c.wait_for("resumed", |m| match m {
        ServerMsg::Frame(f) if f.id == id && screen_text(f).contains("args: --resume 1234abcd") => Some(screen_text(f)),
        _ => None,
    });
    assert!(text.contains("--resume 1234abcd-0000-4000-8000-000000000001 --dangerously-skip-permissions"), "{text}");
    assert!(!text.split("args: --resume").nth(1).unwrap_or_default().lines().next().unwrap_or_default().contains("first task"));
    c.send(ClientMsg::Shutdown);
}

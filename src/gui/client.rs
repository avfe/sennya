//! The window's connection to a daemon: the local one, or one on another machine over SSH.

use std::io::{self, BufReader, BufWriter, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;

use winit::event_loop::EventLoopProxy;

use crate::protocol::{read_msg, write_msg, ClientMsg, ServerMsg, MIN_DAEMON_VERSION, PROTOCOL_VERSION};

pub use crate::client::{connect_or_spawn, shutdown_daemon, spawn_daemon};

#[derive(Debug, Clone, Copy)]
pub enum UserEvent {
    /// Messages from the daemon are waiting in the inbox.
    Wakeup,
    Disconnected,
    ConfigChanged,
    /// The running daemon is too old for this window (its pid). Nothing else is read.
    OutdatedDaemon(u32),
    /// A background reconnect succeeded; the connection waits in the app's slot.
    Connected,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Local,
    /// A daemon on another machine, reached with `ssh <host> sennya proxy`.
    Remote(String),
}

impl Target {
    /// `--remote HOST` on the command line.
    pub fn from_args() -> Target {
        let args: Vec<String> = std::env::args().collect();
        match args.iter().position(|a| a == "--remote").and_then(|i| args.get(i + 1)) {
            Some(host) => Target::Remote(host.clone()),
            None => Target::Local,
        }
    }

    pub fn host(&self) -> Option<&str> {
        match self {
            Target::Local => None,
            Target::Remote(host) => Some(host),
        }
    }
}

pub struct Conn {
    writer: Mutex<Box<dyn Write + Send>>,
    /// The `ssh` process of a remote connection.
    child: Mutex<Option<Child>>,
}

impl Conn {
    pub fn send(&self, msg: &ClientMsg) -> bool {
        let mut w = self.writer.lock().unwrap();
        write_msg(&mut *w, msg).and_then(|_| w.flush()).is_ok()
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.lock().unwrap().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

type Halves = (Box<dyn Read + Send>, Box<dyn Write + Send>, Option<Child>);

fn open(target: &Target) -> io::Result<Halves> {
    match target {
        Target::Local => {
            let stream = connect_or_spawn()?;
            let read = stream.try_clone()?;
            Ok((Box::new(read), Box::new(stream), None))
        }
        Target::Remote(host) => {
            // `SENNYA_SSH` replaces `ssh` (tests use a script that runs the proxy locally).
            let ssh = std::env::var("SENNYA_SSH").unwrap_or_else(|_| "ssh".into());
            // A failed `exec` ends a non-interactive shell, so pick the binary first.
            let remote = "if [ -x \"$HOME/.sennya/bin/sennya\" ]; then exec \"$HOME/.sennya/bin/sennya\" proxy; else exec sennya proxy; fi";
            let mut child = Command::new(ssh)
                .args(["-T", "-C", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10"])
                .args(["-o", "ServerAliveInterval=10", "-o", "ServerAliveCountMax=3"])
                .arg(host)
                .arg(remote)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()?;
            let stdin = child.stdin.take().ok_or_else(|| io::Error::other("no ssh stdin"))?;
            let stdout = child.stdout.take().ok_or_else(|| io::Error::other("no ssh stdout"))?;
            Ok((Box::new(stdout), Box::new(stdin), Some(child)))
        }
    }
}

/// Connect and start the reader thread that feeds `inbox` and wakes the event loop.
pub fn connect(
    target: &Target,
    proxy: EventLoopProxy<UserEvent>,
    inbox: Sender<ServerMsg>,
    pending: Arc<AtomicBool>,
) -> io::Result<Conn> {
    let (read_half, write_half, child) = open(target)?;
    thread::Builder::new().name("daemon-reader".into()).spawn(move || {
        let mut reader = BufReader::with_capacity(256 * 1024, read_half);
        // The greeting is the same in every version; check it before decoding anything else.
        match read_msg::<_, ServerMsg>(&mut reader) {
            Ok(ServerMsg::Hello { version, pid }) if version < MIN_DAEMON_VERSION => {
                let _ = proxy.send_event(UserEvent::OutdatedDaemon(pid));
                return;
            }
            Ok(msg) => {
                let _ = inbox.send(msg);
            }
            Err(_) => {
                let _ = proxy.send_event(UserEvent::Disconnected);
                return;
            }
        }
        loop {
            match read_msg::<_, ServerMsg>(&mut reader) {
                Ok(msg) => {
                    if inbox.send(msg).is_err() {
                        return;
                    }
                    if !pending.swap(true, Ordering::AcqRel) {
                        let _ = proxy.send_event(UserEvent::Wakeup);
                    }
                }
                Err(_) => {
                    let _ = proxy.send_event(UserEvent::Disconnected);
                    return;
                }
            }
        }
    })?;
    let conn = Conn {
        writer: Mutex::new(Box::new(BufWriter::with_capacity(64 * 1024, write_half))),
        child: Mutex::new(child),
    };
    conn.send(&ClientMsg::Hello { version: PROTOCOL_VERSION });
    Ok(conn)
}

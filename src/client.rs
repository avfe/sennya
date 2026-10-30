//! Reaching the daemon: start it when needed, and bridge it over stdio for remote use.

use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use crate::paths;
use crate::protocol::{write_msg, ClientMsg};

/// Start a helper program (a notification, `open`, another window) and reap it when it
/// exits, so finished helpers do not pile up as zombie processes.
pub fn spawn_detached(cmd: &mut Command) -> io::Result<()> {
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
    thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

pub fn spawn_daemon() -> io::Result<()> {
    paths::ensure_private_dir(&paths::data_dir())?;
    let exe = std::env::current_exe()?;
    let log = OpenOptions::new().create(true).append(true).mode(0o600).open(paths::log_file())?;
    let mut cmd = Command::new(exe);
    cmd.arg("daemon").stdin(Stdio::null()).stdout(Stdio::null()).stderr(log);
    unsafe {
        // Own session: the daemon must not die with the GUI or the terminal that started it.
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = cmd.spawn()?;
    thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

pub fn connect_or_spawn() -> io::Result<UnixStream> {
    let sock = paths::socket_path();
    if let Ok(stream) = UnixStream::connect(&sock) {
        return Ok(stream);
    }
    spawn_daemon()?;
    let mut last_err = io::Error::other("daemon did not start");
    for _ in 0..150 {
        thread::sleep(Duration::from_millis(20));
        match UnixStream::connect(&sock) {
            Ok(stream) => return Ok(stream),
            Err(err) => last_err = err,
        }
    }
    Err(last_err)
}

/// `sennya kill`: stop the daemon and every shell.
pub fn shutdown_daemon() -> io::Result<()> {
    let mut stream = UnixStream::connect(paths::socket_path())?;
    write_msg(&mut stream, &ClientMsg::Shutdown)?;
    stream.flush()
}

/// `sennya proxy`: connect stdin/stdout to the daemon (starting it if needed).
/// A window on another machine runs `ssh host sennya proxy` and talks to the daemon through it.
pub fn proxy() -> io::Result<()> {
    let stream = connect_or_spawn()?;
    let mut to_daemon = stream.try_clone()?;
    let mut from_daemon = stream;
    let upstream = thread::spawn(move || {
        let mut stdin = io::stdin().lock();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if to_daemon.write_all(&buf[..n]).is_err() {
                        break;
                    }
                }
            }
        }
        let _ = to_daemon.shutdown(std::net::Shutdown::Both);
    });
    let mut stdout = io::stdout().lock();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        match from_daemon.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if stdout.write_all(&buf[..n]).and_then(|_| stdout.flush()).is_err() {
                    break;
                }
            }
        }
    }
    drop(upstream);
    Ok(())
}

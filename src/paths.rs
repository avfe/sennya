//! Filesystem locations. Everything sennya writes lives in private (0700) directories.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::sys;

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(std::env::temp_dir)
}

/// Persistent state: socket, workspace, saved scrollback. `SENNYA_DATA_DIR` overrides it,
/// which allows running several independent instances.
pub fn data_dir() -> PathBuf {
    match std::env::var_os("SENNYA_DATA_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        // %LOCALAPPDATA% on Windows: the socket and state must not roam between machines.
        _ => dirs::data_local_dir().unwrap_or_else(|| home().join(".local/share")).join("sennya"),
    }
}

/// `~/.config/sennya` on every unix, including macOS (where dotfile users expect it).
pub fn config_dir() -> PathBuf {
    match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir).join("sennya"),
        _ => home().join(".config").join("sennya"),
    }
}

pub fn config_file() -> PathBuf {
    config_dir().join("config.toml")
}

/// Unix socket paths are limited to ~104 bytes. When the data dir is too deep, the socket
/// lives in `/tmp/sennya-<uid>/` instead (created 0700 and checked to be ours).
pub fn socket_path() -> PathBuf {
    let path = data_dir().join("sennya.sock");
    if path.as_os_str().len() < 100 {
        return path;
    }
    let uid = sys::user_id();
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in data_dir().as_os_str().as_encoded_bytes() {
        hash = (hash ^ *b as u64).wrapping_mul(0x100000001b3);
    }
    #[cfg(unix)]
    let tmp = PathBuf::from("/tmp");
    #[cfg(windows)]
    let tmp = std::env::temp_dir();
    tmp.join(format!("sennya-{uid}")).join(format!("{hash:016x}.sock"))
}

/// Create the socket's directory, refusing directories owned by someone else.
pub fn ensure_socket_dir() -> io::Result<()> {
    let sock = socket_path();
    let Some(dir) = sock.parent() else { return Ok(()) };
    ensure_private_dir(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = fs::metadata(dir)?;
        if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, format!("unsafe socket directory {}", dir.display())));
        }
    }
    Ok(())
}

pub fn state_file() -> PathBuf {
    data_dir().join("state.json")
}

pub fn window_file() -> PathBuf {
    data_dir().join("window.json")
}

pub fn scrollback_dir() -> PathBuf {
    data_dir().join("scrollback")
}

pub fn scrollback_file(id: u64) -> PathBuf {
    scrollback_dir().join(format!("{id}.ansi"))
}

pub fn log_file() -> PathBuf {
    data_dir().join("daemon.log")
}

/// Pasted screenshots are written here so their path can be handed to CLI tools.
pub fn paste_dir() -> PathBuf {
    dirs::cache_dir().unwrap_or_else(|| home().join(".cache")).join("sennya").join("pastes")
}

pub fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    sys::make_private_dir(dir)
}

/// Write a file atomically with 0600 permissions (scrollback may contain secrets).
pub fn write_private(path: &Path, data: &[u8]) -> io::Result<()> {
    use std::io::Write;
    // A unique temp name: two saves of the same file must never write into one temp file.
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = path.with_extension(format!("{}.{n}.tmp", std::process::id()));
    {
        let mut f = sys::private_file().write(true).create(true).truncate(true).open(&tmp)?;
        f.write_all(data)?;
        f.sync_data()?;
    }
    fs::rename(&tmp, path)
}

/// `~/Projects/foo` instead of `/Users/me/Projects/foo`.
pub fn tilde(path: &str) -> String {
    let home = home();
    let home = home.to_string_lossy();
    if !home.is_empty() && path.starts_with(home.as_ref()) {
        format!("~{}", &path[home.len()..])
    } else {
        path.to_string()
    }
}

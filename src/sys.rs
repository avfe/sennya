//! What differs between unix and Windows: the daemon socket, background processes,
//! private files, and stopping or checking a process by pid.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;
use std::process::{Child, Command};

#[cfg(unix)]
pub use std::os::unix::net::{UnixListener, UnixStream};
#[cfg(windows)]
pub use uds_windows::{UnixListener, UnixStream};

#[cfg(windows)]
mod win {
    pub const DETACHED_PROCESS: u32 = 0x0000_0008;
    pub const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    pub const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;
}

/// Start a process that outlives the window or terminal that started it: its own session
/// on unix, no console and out of the caller's job on Windows.
pub fn spawn_background(cmd: &mut Command) -> io::Result<Child> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        cmd.spawn()
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use win::*;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB);
        cmd.spawn().or_else(|_| {
            // Some jobs (an SSH session, for one) do not allow leaving them.
            cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
            cmd.spawn()
        })
    }
}

/// Keep a console helper (`cmd /c start`) from flashing a window on Windows.
pub fn hide_console(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(win::CREATE_NO_WINDOW);
    }
    let _ = cmd;
}

/// Options that create files readable only by this user (0600). On Windows the files live
/// in the user's profile, which other users cannot read.
pub fn private_file() -> OpenOptions {
    #[allow(unused_mut)]
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

/// Make a directory private (0700). A no-op on Windows, see `private_file`.
pub fn make_private_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let _ = dir;
    Ok(())
}

/// Make a file private (0600). A no-op on Windows.
pub fn make_private_file(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    let _ = path;
    Ok(())
}

/// Lock `path` for as long as the returned file stays open. `None` when another process
/// holds the lock.
pub fn try_lock(path: &Path) -> io::Result<Option<File>> {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let file = OpenOptions::new().create(true).truncate(false).write(true).open(path)?;
        let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
        Ok(locked.then_some(file))
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const ERROR_SHARING_VIOLATION: i32 = 32;
        // No sharing: a second open fails while the first one is alive.
        match OpenOptions::new().create(true).truncate(false).write(true).share_mode(0).open(path) {
            Ok(file) => Ok(Some(file)),
            Err(err) if err.raw_os_error() == Some(ERROR_SHARING_VIOLATION) => Ok(None),
            Err(err) => Err(err),
        }
    }
}

pub fn process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
        use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(handle, &mut code) != 0;
        CloseHandle(handle);
        ok && code == STILL_ACTIVE as u32
    }
}

/// Ask a daemon to save and exit (SIGTERM). Windows has no signals; there the caller sends
/// `ClientMsg::SaveAndExit` instead, and this returns false.
pub fn terminate(pid: u32) -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as i32, libc::SIGTERM) == 0 }
    }
    #[cfg(windows)]
    {
        let _ = pid;
        false
    }
}

/// A stable id of the current user for naming shared temp folders.
pub fn user_id() -> String {
    #[cfg(unix)]
    {
        unsafe { libc::geteuid() }.to_string()
    }
    #[cfg(windows)]
    {
        std::env::var("USERNAME").unwrap_or_else(|_| "user".into())
    }
}

/// Open a file or URL with the system's default program.
pub fn open_command(target: &std::ffi::OsStr) -> Command {
    #[cfg(target_os = "macos")]
    let cmd = {
        let mut cmd = Command::new("open");
        cmd.arg(target);
        cmd
    };
    #[cfg(windows)]
    let cmd = {
        // Not `cmd /c start`: cmd would run whatever follows a `&` in a link.
        let mut cmd = Command::new("rundll32");
        cmd.arg("url.dll,FileProtocolHandler").arg(target);
        cmd
    };
    #[cfg(not(any(target_os = "macos", windows)))]
    let cmd = {
        let mut cmd = Command::new("xdg-open");
        cmd.arg(target);
        cmd
    };
    cmd
}

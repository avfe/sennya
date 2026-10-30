//! Process introspection: working directory and foreground job of a shell.

use std::os::fd::RawFd;

pub fn foreground_pid(master: RawFd) -> Option<i32> {
    let pgid = unsafe { libc::tcgetpgrp(master) };
    (pgid > 0).then_some(pgid)
}

#[cfg(target_os = "macos")]
pub fn cwd(pid: i32) -> Option<String> {
    use std::ffi::CStr;
    unsafe {
        let mut info: libc::proc_vnodepathinfo = std::mem::zeroed();
        let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as i32;
        let n = libc::proc_pidinfo(
            pid,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        );
        if n != size {
            return None;
        }
        let ptr = info.pvi_cdir.vip_path.as_ptr() as *const libc::c_char;
        let path = CStr::from_ptr(ptr).to_string_lossy().into_owned();
        (!path.is_empty()).then_some(path)
    }
}

#[cfg(target_os = "macos")]
pub fn name(pid: i32) -> Option<String> {
    let mut buf = [0u8; 256];
    let n = unsafe { libc::proc_name(pid, buf.as_mut_ptr() as *mut libc::c_void, buf.len() as u32) };
    (n > 0).then(|| String::from_utf8_lossy(&buf[..n as usize]).into_owned())
}

#[cfg(target_os = "macos")]
pub fn path(pid: i32) -> Option<String> {
    let mut buf = [0u8; 4096];
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr() as *mut libc::c_void, buf.len() as u32) };
    (n > 0).then(|| String::from_utf8_lossy(&buf[..n as usize]).into_owned())
}

#[cfg(not(target_os = "macos"))]
pub fn path(pid: i32) -> Option<String> {
    std::fs::read_link(format!("/proc/{pid}/exe")).ok().map(|p| p.to_string_lossy().into_owned())
}

/// Command line of a process.
#[cfg(target_os = "macos")]
pub fn args(pid: i32) -> Option<Vec<String>> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut size: libc::size_t = 0;
    unsafe {
        if libc::sysctl(mib.as_mut_ptr(), 3, std::ptr::null_mut(), &mut size, std::ptr::null_mut(), 0) != 0 {
            return None;
        }
    }
    let mut buf = vec![0u8; size];
    unsafe {
        if libc::sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr() as *mut libc::c_void, &mut size, std::ptr::null_mut(), 0) != 0 {
            return None;
        }
    }
    buf.truncate(size);
    // Layout: argc, the executable path, NUL padding, then argc NUL-terminated strings.
    let argc = i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?) as usize;
    let mut rest = &buf[4..];
    rest = &rest[rest.iter().position(|b| *b == 0)?..];
    rest = &rest[rest.iter().position(|b| *b != 0)?..];
    let mut out = Vec::with_capacity(argc);
    for _ in 0..argc {
        let end = rest.iter().position(|b| *b == 0)?;
        out.push(String::from_utf8_lossy(&rest[..end]).into_owned());
        rest = &rest[(end + 1).min(rest.len())..];
    }
    Some(out)
}

#[cfg(not(target_os = "macos"))]
pub fn args(pid: i32) -> Option<Vec<String>> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    Some(raw.split(|b| *b == 0).filter(|a| !a.is_empty()).map(|a| String::from_utf8_lossy(a).into_owned()).collect())
}

/// The options of a `claude` command line that a resumed chat should keep. Prompts and
/// `--resume`/`--continue` are dropped so resuming never re-sends anything.
pub fn claude_flags(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        let (name, inline) = match a.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n, Some(v.to_string())),
            _ => (a.as_str(), None),
        };
        match name {
            "--dangerously-skip-permissions" | "--verbose" => out.push(name.to_string()),
            "--model" | "--permission-mode" | "--add-dir" | "--fallback-model" | "--settings" | "--mcp-config" => {
                if let Some(v) = inline.or_else(|| it.next().cloned()) {
                    out.push(name.to_string());
                    out.push(v);
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_safe_claude_flags() {
        let args: Vec<String> = ["claude", "--dangerously-skip-permissions", "--model", "opus", "do the task", "--resume", "abc", "--permission-mode=plan"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(claude_flags(&args), vec!["--dangerously-skip-permissions", "--model", "opus", "--permission-mode", "plan"]);
        assert!(is_shell("-zsh") && is_shell("bash") && !is_shell("claude"));
        let resumed: Vec<String> = ["claude", "--resume", "abc-1", "--model", "x"].iter().map(|s| s.to_string()).collect();
        assert_eq!(claude_resume_id(&resumed).as_deref(), Some("abc-1"));
        assert_eq!(claude_resume_id(&args[..2]), None);
        let me = args_of_self();
        assert!(!me.is_empty());
    }

    fn args_of_self() -> Vec<String> {
        args(std::process::id() as i32).unwrap_or_default()
    }
}

/// Direct children of a process.
#[cfg(target_os = "macos")]
pub fn children(pid: i32) -> Vec<i32> {
    let mut buf = vec![0i32; 256];
    let n = unsafe { libc::proc_listchildpids(pid, buf.as_mut_ptr() as *mut libc::c_void, (buf.len() * 4) as libc::c_int) };
    if n <= 0 {
        return Vec::new();
    }
    buf.truncate((n as usize).min(256));
    buf.retain(|p| *p > 0);
    buf
}

#[cfg(not(target_os = "macos"))]
pub fn children(pid: i32) -> Vec<i32> {
    std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))
        .map(|s| s.split_whitespace().filter_map(|p| p.parse().ok()).collect())
        .unwrap_or_default()
}

/// The chat a `claude --resume ID` command line reopens.
pub fn claude_resume_id(args: &[String]) -> Option<String> {
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        if let Some(v) = a.strip_prefix("--resume=") {
            return Some(v.to_string());
        }
        if a == "--resume" || a == "-r" {
            return it.next().filter(|v| !v.starts_with('-')).cloned();
        }
    }
    None
}

/// Whether a process name is a shell (or the login wrapper) rather than a program in it.
pub fn is_shell(name: &str) -> bool {
    let name = name.trim_start_matches('-');
    matches!(name, "zsh" | "bash" | "fish" | "sh" | "dash" | "ksh" | "tcsh" | "csh" | "nu" | "xonsh" | "elvish" | "login")
}

/// A readable program name. Some tools install their binary under its version
/// (`~/.local/share/claude/versions/2.1.312`), so the raw name would be `2.1.312`.
pub fn display_name(pid: i32) -> Option<String> {
    let name = name(pid)?;
    let looks_like_version = name.chars().all(|c| c.is_ascii_digit() || c == '.');
    if looks_like_version {
        if let Some(path) = path(pid) {
            let parts: Vec<&str> = path.split('/').collect();
            if let Some(i) = parts.iter().position(|p| *p == "versions") {
                if i > 0 {
                    return Some(parts[i - 1].to_string());
                }
            }
        }
    }
    Some(name)
}

#[cfg(not(target_os = "macos"))]
pub fn cwd(pid: i32) -> Option<String> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok().map(|p| p.to_string_lossy().into_owned())
}

#[cfg(not(target_os = "macos"))]
pub fn name(pid: i32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/comm")).ok().map(|s| s.trim().to_string())
}

//! Process introspection: working directory and foreground job of a shell.

#[cfg(unix)]
use std::os::fd::RawFd;

#[cfg(windows)]
pub use win::{args, children, cwd, foreground_pid, name, path};

#[cfg(unix)]
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

#[cfg(all(unix, not(target_os = "macos")))]
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

#[cfg(all(unix, not(target_os = "macos")))]
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

#[cfg(all(unix, not(target_os = "macos")))]
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
    matches!(
        name,
        "zsh" | "bash" | "fish" | "sh" | "dash" | "ksh" | "tcsh" | "csh" | "nu" | "xonsh" | "elvish" | "login" | "powershell" | "pwsh" | "cmd"
    )
}

/// A readable program name. Some tools install their binary under its version
/// (`~/.local/share/claude/versions/2.1.312`), so the raw name would be `2.1.312`.
pub fn display_name(pid: i32) -> Option<String> {
    let name = name(pid)?;
    // An npm install runs as `node .../@anthropic-ai/claude-code/cli.js`.
    if name == "node" && args(pid).is_some_and(|a| a.iter().skip(1).take(2).any(|a| a.contains("claude-code"))) {
        return Some("claude".into());
    }
    let looks_like_version = name.chars().all(|c| c.is_ascii_digit() || c == '.');
    if looks_like_version {
        if let Some(path) = path(pid) {
            let parts: Vec<&str> = path.split(['/', '\\']).collect();
            if let Some(i) = parts.iter().position(|p| *p == "versions") {
                if i > 0 {
                    return Some(parts[i - 1].to_string());
                }
            }
        }
    }
    Some(name)
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn cwd(pid: i32) -> Option<String> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok().map(|p| p.to_string_lossy().into_owned())
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn name(pid: i32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/comm")).ok().map(|s| s.trim().to_string())
}

/// Windows has no foreground process group; the program a shell runs is its child.
#[cfg(windows)]
mod win {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use windows_sys::Wdk::System::Threading::{NtQueryInformationProcess, ProcessCommandLineInformation};
    use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, INVALID_HANDLE_VALUE, UNICODE_STRING};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION};
    use windows_sys::Win32::UI::Shell::CommandLineToArgvW;

    struct Proc {
        pid: u32,
        parent: u32,
        name: String,
    }

    /// Every process, re-read at most every 250 ms: each terminal asks every second.
    fn with_processes<T>(f: impl FnOnce(&[Proc]) -> T) -> T {
        static CACHE: Mutex<Option<(Instant, Vec<Proc>)>> = Mutex::new(None);
        let mut cache = CACHE.lock().unwrap();
        if cache.as_ref().is_none_or(|(at, _)| at.elapsed() > Duration::from_millis(250)) {
            *cache = Some((Instant::now(), snapshot()));
        }
        f(&cache.as_ref().unwrap().1)
    }

    fn snapshot() -> Vec<Proc> {
        let mut out = Vec::new();
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snap == INVALID_HANDLE_VALUE {
                return out;
            }
            let mut e: PROCESSENTRY32W = std::mem::zeroed();
            e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            let mut ok = Process32FirstW(snap, &mut e) != 0;
            while ok {
                let len = e.szExeFile.iter().position(|c| *c == 0).unwrap_or(e.szExeFile.len());
                let exe = String::from_utf16_lossy(&e.szExeFile[..len]);
                let name = match exe.len().checked_sub(4) {
                    Some(i) if exe[i..].eq_ignore_ascii_case(".exe") => exe[..i].to_string(),
                    _ => exe,
                };
                out.push(Proc { pid: e.th32ProcessID, parent: e.th32ParentProcessID, name });
                ok = Process32NextW(snap, &mut e) != 0;
            }
            CloseHandle(snap);
        }
        out
    }

    /// The program the user runs in the shell `shell`: down through nested shells to the
    /// first other program, else the shell itself.
    pub fn foreground_pid(shell: i32) -> Option<i32> {
        with_processes(|procs| {
            let mut cur = shell as u32;
            for _ in 0..8 {
                let Some(child) = procs.iter().rfind(|p| p.parent == cur && p.pid != cur && p.name != "conhost") else { break };
                cur = child.pid;
                if !super::is_shell(&child.name.to_ascii_lowercase()) {
                    break;
                }
            }
            Some(cur as i32)
        })
    }

    pub fn children(pid: i32) -> Vec<i32> {
        with_processes(|procs| procs.iter().filter(|p| p.parent == pid as u32 && p.pid != pid as u32).map(|p| p.pid as i32).collect())
    }

    pub fn name(pid: i32) -> Option<String> {
        with_processes(|procs| procs.iter().find(|p| p.pid == pid as u32).map(|p| p.name.to_ascii_lowercase()))
    }

    /// Reading another process's working directory needs its memory; sennya keeps the folder
    /// the terminal started in instead.
    pub fn cwd(_pid: i32) -> Option<String> {
        None
    }

    pub fn path(pid: i32) -> Option<String> {
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid as u32);
            if h.is_null() {
                return None;
            }
            let mut buf = [0u16; 1024];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len) != 0;
            CloseHandle(h);
            ok.then(|| String::from_utf16_lossy(&buf[..len as usize]))
        }
    }

    pub fn args(pid: i32) -> Option<Vec<String>> {
        const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004_u32 as i32;
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid as u32);
            if h.is_null() {
                return None;
            }
            // u64s keep the UNICODE_STRING header aligned.
            let mut buf = vec![0u64; 1024];
            let mut len = 0u32;
            let mut status = NtQueryInformationProcess(h, ProcessCommandLineInformation, buf.as_mut_ptr().cast(), (buf.len() * 8) as u32, &mut len);
            if status == STATUS_INFO_LENGTH_MISMATCH {
                buf = vec![0u64; len as usize / 8 + 1];
                status = NtQueryInformationProcess(h, ProcessCommandLineInformation, buf.as_mut_ptr().cast(), (buf.len() * 8) as u32, &mut len);
            }
            CloseHandle(h);
            if status < 0 {
                return None;
            }
            let us = &*(buf.as_ptr() as *const UNICODE_STRING);
            if us.Buffer.is_null() {
                return Some(Vec::new());
            }
            let mut line: Vec<u16> = std::slice::from_raw_parts(us.Buffer, us.Length as usize / 2).to_vec();
            line.push(0);
            let mut argc = 0;
            let argv = CommandLineToArgvW(line.as_ptr(), &mut argc);
            if argv.is_null() {
                return None;
            }
            let mut out = Vec::with_capacity(argc as usize);
            for i in 0..argc as usize {
                let arg = *argv.add(i);
                let n = (0..).take_while(|&j| *arg.add(j) != 0).count();
                out.push(String::from_utf16_lossy(std::slice::from_raw_parts(arg, n)));
            }
            LocalFree(argv.cast());
            Some(out)
        }
    }
}

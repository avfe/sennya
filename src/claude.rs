//! Index of Claude Code conversations (`~/.claude/projects/<folder>/<session-id>.jsonl`),
//! so terminals can remember and resume them.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ClaudeSession {
    /// The id for `claude --resume`.
    pub id: String,
    pub cwd: String,
    /// Custom title, else Claude's own title for the chat, else the first prompt.
    pub title: String,
    pub first_prompt: String,
    pub last_prompt: String,
    pub branch: String,
    /// Last write, unix seconds.
    pub modified: u64,
    /// Name of the sennya terminal it last ran in, if any.
    pub terminal: String,
}

pub fn projects_dir() -> PathBuf {
    match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir).join("projects"),
        _ => dirs::home_dir().unwrap_or_default().join(".claude").join("projects"),
    }
}

/// Claude Code names a project folder after its path with every non-alphanumeric char as `-`.
pub fn encode_cwd(cwd: &str) -> String {
    cwd.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

fn read_range(file: &mut File, start: u64, len: usize) -> String {
    let mut buf = vec![0u8; len];
    if file.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let n = file.read(&mut buf).unwrap_or(0);
    buf.truncate(n);
    String::from_utf8_lossy(&buf).into_owned()
}

fn prompt_text(v: &serde_json::Value) -> Option<String> {
    let content = v.get("message")?.get("content")?;
    let text = match content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(items) => items
            .iter()
            .filter(|i| i.get("type").and_then(|t| t.as_str()) == Some("text"))
            .filter_map(|i| i.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join(" "),
        _ => return None,
    };
    let text = text.trim();
    // Slash command echoes and tool results are not prompts.
    (!text.is_empty() && !text.starts_with('<') && !text.starts_with("Caveat:")).then(|| one_line(text, 160))
}

fn one_line(text: &str, max: usize) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > max {
        format!("{}…", flat.chars().take(max).collect::<String>())
    } else {
        flat
    }
}

/// Read the head and tail of a transcript; transcripts can be many megabytes.
pub fn parse(path: &Path) -> Option<ClaudeSession> {
    let id = path.file_stem()?.to_str()?.to_string();
    let mut file = File::open(path).ok()?;
    let meta = file.metadata().ok()?;
    let size = meta.len();
    let modified = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs();
    const HEAD: usize = 96 * 1024;
    const TAIL: usize = 256 * 1024;
    let head = read_range(&mut file, 0, HEAD);
    let tail = if size as usize > HEAD { read_range(&mut file, size.saturating_sub(TAIL as u64).max(HEAD as u64), TAIL) } else { String::new() };

    let mut s = ClaudeSession { id, modified, ..Default::default() };
    let (mut ai_title, mut custom_title, mut summary) = (String::new(), String::new(), String::new());
    for (part, is_head) in [(&head, true), (&tail, false)] {
        for line in part.lines() {
            let interesting = line.contains("\"type\":\"user\"")
                || line.contains("\"ai-title\"")
                || line.contains("\"custom-title\"")
                || line.contains("\"type\":\"summary\"")
                || line.contains("\"last-prompt\"");
            if !interesting {
                continue;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
            let str_of = |key: &str| v.get(key).and_then(|x| x.as_str()).unwrap_or_default().to_string();
            match v.get("type").and_then(|t| t.as_str()) {
                Some("user") => {
                    if s.cwd.is_empty() {
                        s.cwd = str_of("cwd");
                    }
                    let branch = str_of("gitBranch");
                    if !branch.is_empty() {
                        s.branch = branch;
                    }
                    if v.get("isMeta").and_then(|m| m.as_bool()) == Some(true) {
                        continue;
                    }
                    if let Some(p) = prompt_text(&v) {
                        if is_head && s.first_prompt.is_empty() {
                            s.first_prompt = p.clone();
                        }
                        s.last_prompt = p;
                    }
                }
                Some("ai-title") => ai_title = str_of("aiTitle"),
                Some("custom-title") => custom_title = str_of("customTitle"),
                Some("summary") => summary = str_of("summary"),
                Some("last-prompt") => {
                    let p = one_line(&str_of("lastPrompt"), 160);
                    if !p.is_empty() {
                        s.last_prompt = p;
                    }
                }
                _ => {}
            }
        }
    }
    if s.cwd.is_empty() && s.first_prompt.is_empty() {
        return None;
    }
    s.title = [custom_title, ai_title, summary, s.first_prompt.clone()]
        .into_iter()
        .map(|t| one_line(&t, 120))
        .find(|t| !t.is_empty())
        .unwrap_or_else(|| "Untitled chat".into());
    Some(s)
}

/// Cached index; files are re-read only when their size or mtime changes.
#[derive(Default)]
pub struct Index {
    cache: HashMap<PathBuf, (u64, SystemTime, Option<ClaudeSession>)>,
}

impl Index {
    fn get(&mut self, path: &Path) -> Option<ClaudeSession> {
        let meta = std::fs::metadata(path).ok()?;
        let (len, mtime) = (meta.len(), meta.modified().ok()?);
        match self.cache.get(path) {
            Some((l, m, s)) if *l == len && *m == mtime => s.clone(),
            _ => {
                let s = parse(path);
                self.cache.insert(path.to_path_buf(), (len, mtime, s.clone()));
                s
            }
        }
    }

    fn transcripts(dir: &Path) -> Vec<(PathBuf, SystemTime)> {
        let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
        entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
            .filter_map(|p| Some((p.clone(), std::fs::metadata(&p).ok()?.modified().ok()?)))
            .collect()
    }

    /// Every conversation, newest first.
    pub fn all(&mut self, limit: usize) -> Vec<ClaudeSession> {
        let mut files: Vec<(PathBuf, SystemTime)> = Vec::new();
        if let Ok(projects) = std::fs::read_dir(projects_dir()) {
            for p in projects.flatten() {
                files.extend(Self::transcripts(&p.path()));
            }
        }
        files.sort_by(|a, b| b.1.cmp(&a.1));
        files.into_iter().filter_map(|(p, _)| self.get(&p)).take(limit).collect()
    }

    /// A conversation by its id, from whichever project folder holds it.
    pub fn find(&mut self, id: &str) -> Option<ClaudeSession> {
        let file = format!("{id}.jsonl");
        let projects = std::fs::read_dir(projects_dir()).ok()?;
        let path = projects.flatten().map(|p| p.path().join(&file)).find(|p| p.exists())?;
        self.get(&path)
    }

    /// The newest conversation for `cwd` written since `since`, skipping `taken` ids.
    pub fn newest_in(&mut self, cwd: &str, since: SystemTime, taken: &[String]) -> Option<ClaudeSession> {
        let dir = projects_dir().join(encode_cwd(cwd));
        let mut files = Self::transcripts(&dir);
        files.retain(|(_, m)| *m >= since);
        files.sort_by(|a, b| b.1.cmp(&a.1));
        files
            .into_iter()
            .filter(|(p, _)| p.file_stem().and_then(|s| s.to_str()).is_some_and(|id| !taken.iter().any(|t| t == id)))
            .find_map(|(p, _)| self.get(&p))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_transcript() {
        let dir = std::env::temp_dir().join(format!("sennya-claude-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("1111-2222.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"type":"user","cwd":"/work/app","gitBranch":"main","message":{"role":"user","content":"<command-name>/clear</command-name>"}}"#, "\n",
                r#"{"type":"user","cwd":"/work/app","gitBranch":"main","message":{"role":"user","content":[{"type":"text","text":"Fix the   login bug"}]}}"#, "\n",
                r#"{"type":"assistant","message":{"content":"ok"}}"#, "\n",
                r#"{"type":"ai-title","aiTitle":"Fix login redirect","sessionId":"1111-2222"}"#, "\n",
            ),
        )
        .unwrap();
        let s = parse(&path).unwrap();
        assert_eq!(s.id, "1111-2222");
        assert_eq!(s.cwd, "/work/app");
        assert_eq!(s.title, "Fix login redirect");
        assert_eq!(s.first_prompt, "Fix the login bug");
        assert_eq!(s.branch, "main");
        assert_eq!(encode_cwd("/Users/me/Projects/my.app"), "-Users-me-Projects-my-app");
        let _ = std::fs::remove_dir_all(dir);
    }
}

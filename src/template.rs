//! `{placeholders}` in profiles: unique tags, dates, environment and answers to prompts.
//!
//! | placeholder        | example                                |
//! |--------------------|----------------------------------------|
//! | `{uuid}`           | `3f2b9c1e-8a4d-4f6b-9e2a-7c1d5b8e0f3a` |
//! | `{short}`          | `3f2b9c1e`                             |
//! | `{words}`          | `brave-otter-42`                       |
//! | `{timestamp}`      | `20261029-231502`                      |
//! | `{date}` `{time}`  | `2026-10-29`, `231502`                 |
//! | `{unix}`           | `1793315702`                           |
//! | `{home}` `{cwd}`   | home folder, folder of the focused terminal |
//! | `{env:NAME}`       | value of an environment variable       |
//! | `{prompt:Label}`   | asked when the profile runs            |
//! | `{cwd_name}`       | last part of `{cwd}`                   |
//!
//! Add `|q` to quote a value for the shell: `claude {prompt:Task|q}`.
//!
//! Every placeholder has one value per run, so `{timestamp}` in two commands matches.
//! `{{` and `}}` produce literal braces.

use std::collections::HashMap;
use std::io::Read;

const ADJECTIVES: &[&str] = &[
    "amber", "brave", "calm", "clever", "cosmic", "crisp", "dusty", "eager", "fancy", "fuzzy", "gentle", "golden",
    "happy", "icy", "jolly", "keen", "lucky", "mellow", "misty", "nimble", "noble", "plucky", "quiet", "rapid",
    "rusty", "shiny", "silent", "snowy", "solar", "swift", "tidy", "vivid", "wild", "witty", "young", "zesty",
];
const NOUNS: &[&str] = &[
    "otter", "falcon", "maple", "comet", "badger", "cedar", "dolphin", "ember", "fjord", "gecko", "harbor", "island",
    "jaguar", "koala", "lagoon", "meadow", "nebula", "orchid", "panda", "quartz", "raven", "summit", "tiger", "tundra",
    "valley", "walrus", "willow", "yak", "zephyr", "lynx", "heron", "canyon", "pebble", "river", "spruce", "atlas",
];

pub fn random_bytes(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    if std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut buf)).is_err() {
        // Extremely unlikely; fall back to time-based noise.
        let mut x = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(1) as u64;
        for b in &mut buf {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            *b = x as u8;
        }
    }
    buf
}

pub fn uuid_v4() -> String {
    let mut b = random_bytes(16);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

pub fn random_words() -> String {
    let b = random_bytes(3);
    format!("{}-{}-{}", ADJECTIVES[b[0] as usize % ADJECTIVES.len()], NOUNS[b[1] as usize % NOUNS.len()], b[2] % 90 + 10)
}

/// Local time as (year, month, day, hour, minute, second).
pub fn local_time(unix: i64) -> (i32, u32, u32, u32, u32, u32) {
    unsafe {
        let t: libc::time_t = unix as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        (tm.tm_year + 1900, tm.tm_mon as u32 + 1, tm.tm_mday as u32, tm.tm_hour as u32, tm.tm_min as u32, tm.tm_sec as u32)
    }
}

/// Labels of `{prompt:Label}` placeholders, in order of appearance.
pub fn prompts<'a>(texts: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for text in texts {
        let mut rest = text;
        while let Some(start) = rest.find("{prompt:") {
            let after = &rest[start + 8..];
            let Some(end) = after.find('}') else { break };
            let label = after[..end].trim_end_matches("|q").to_string();
            if !out.contains(&label) {
                out.push(label);
            }
            rest = &after[end + 1..];
        }
    }
    out
}

pub struct Template {
    vars: HashMap<String, String>,
}

impl Template {
    /// Fresh values for one run. `extra` adds or overrides variables (`cwd`, `prompt:Label`, ...).
    pub fn new(extra: &[(String, String)]) -> Template {
        let uuid = uuid_v4();
        let unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
        let (y, mo, d, h, mi, s) = local_time(unix);
        let mut vars = HashMap::new();
        vars.insert("short".into(), uuid[..8].to_string());
        vars.insert("uuid".into(), uuid);
        vars.insert("words".into(), random_words());
        vars.insert("timestamp".into(), format!("{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}"));
        vars.insert("date".into(), format!("{y:04}-{mo:02}-{d:02}"));
        vars.insert("time".into(), format!("{h:02}{mi:02}{s:02}"));
        vars.insert("unix".into(), unix.to_string());
        vars.insert("home".into(), dirs::home_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default());
        for (k, v) in extra {
            vars.insert(k.clone(), v.clone());
        }
        if let Some(cwd) = vars.get("cwd").cloned() {
            vars.insert("cwd_name".into(), cwd.trim_end_matches('/').rsplit('/').next().unwrap_or_default().to_string());
        }
        Template { vars }
    }

    pub fn expand(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(i) = rest.find(['{', '}']) {
            out.push_str(&rest[..i]);
            let tail = &rest[i..];
            if tail.starts_with("{{") || tail.starts_with("}}") {
                out.push_str(&tail[..1]);
                rest = &tail[2..];
                continue;
            }
            if tail.starts_with('{') {
                if let Some(end) = tail.find('}') {
                    let raw = &tail[1..end];
                    let (key, quote) = match raw.strip_suffix("|q") {
                        Some(k) => (k, true),
                        None => (raw, false),
                    };
                    let value = if let Some(name) = key.strip_prefix("env:") {
                        std::env::var(name).ok()
                    } else {
                        self.vars.get(key).cloned()
                    };
                    let value = value.map(|v| if quote { shell_quote(&v) } else { v });
                    match value {
                        Some(v) => out.push_str(&v),
                        None => out.push_str(&tail[..=end]),
                    }
                    rest = &tail[end + 1..];
                    continue;
                }
            }
            out.push_str(&tail[..1]);
            rest = &tail[1..];
        }
        out.push_str(rest);
        out
    }
}

/// Single-quote for POSIX shells; empty stays empty so an unanswered prompt adds nothing.
pub fn shell_quote(s: &str) -> String {
    if s.is_empty() {
        return String::new();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Quote for the shell only when the value has characters the shell would interpret.
pub fn shell_word(s: &str) -> String {
    let plain = !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:@%+,".contains(c));
    if plain {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// `~/x` -> `/Users/me/x`.
pub fn expand_tilde(path: &str) -> String {
    match (path.strip_prefix('~'), dirs::home_dir()) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') => format!("{}{rest}", home.to_string_lossy()),
        _ => path.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_consistently() {
        let t = Template::new(&[("prompt:Task".into(), "fix-login".into())]);
        let a = t.expand("~/tasks/{prompt:Task}-{timestamp}");
        let b = t.expand("cd ~/tasks/{prompt:Task}-{timestamp}");
        assert!(a.starts_with("~/tasks/fix-login-20"));
        assert_eq!(b, format!("cd {a}"));
        assert_eq!(t.expand("{{literal}} {unknown}"), "{literal} {unknown}");
        assert_eq!(t.expand("{short}").len(), 8);
        assert_eq!(t.expand("{uuid}").len(), 36);
        assert_eq!(t.expand("{words}").split('-').count(), 3);
        assert_eq!(prompts(["{prompt:A} {prompt:B}", "{prompt:A|q}"]), vec!["A", "B"]);
        let t = Template::new(&[("prompt:Task".into(), "it's done".into()), ("cwd".into(), "/w/app/".into())]);
        assert_eq!(t.expand("claude {prompt:Task|q}"), "claude 'it'\\''s done'");
        assert_eq!(t.expand("{cwd_name}"), "app");
    }
}

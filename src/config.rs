//! User configuration (`~/.config/sennya/config.toml`) and built-in color themes.

use std::collections::BTreeMap;
use std::fs;

use serde::{Deserialize, Serialize};

use crate::paths;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub font: FontConfig,
    pub theme: ThemeConfig,
    pub shell: ShellConfig,
    /// Extra environment variables for every shell.
    pub env: BTreeMap<String, String>,
    pub terminal: TerminalConfig,
    pub clipboard: ClipboardConfig,
    pub persistence: PersistenceConfig,
    pub ui: UiConfig,
    pub security: SecurityConfig,
    /// Keybinding overrides: action name -> chord, e.g. `new_terminal = "cmd+t"`.
    pub keys: BTreeMap<String, String>,
    /// `[[profile]]` entries: one-key recipes that open a terminal and run commands.
    #[serde(rename = "profile")]
    pub profiles: Vec<Profile>,
    pub claude: ClaudeConfig,
    /// `[[remote]]` servers with a sennya daemon (`sennya remote install HOST`).
    #[serde(rename = "remote")]
    pub remotes: Vec<RemoteHost>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClaudeConfig {
    /// Close Claude Code in a terminal that is out of sight and idle for this many minutes,
    /// and resume the chat when the terminal is opened again. Saves ~250 MB per chat. 0 = never.
    pub hibernate_after_mins: u64,
}

impl Default for ClaudeConfig {
    fn default() -> Self {
        Self { hibernate_after_mins: 30 }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RemoteHost {
    pub name: String,
    /// What you would type after `ssh` (a host from ~/.ssh/config, or user@host).
    pub host: String,
}

/// One command of a profile: a plain string, or a table with options.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Step {
    Run(String),
    Detailed {
        run: String,
        /// Wait until the cursor line ends with this text (a prompt like `$`) before typing.
        /// Alternatives are separated by `|`: `"$|%|#"`.
        #[serde(default)]
        wait_for: Option<String>,
        /// Wait until this text appears anywhere on the screen.
        #[serde(default)]
        wait_text: Option<String>,
        /// Extra pause before typing, in milliseconds.
        #[serde(default)]
        delay_ms: u64,
        /// Give up waiting after this long (default 60 s).
        #[serde(default)]
        timeout_ms: Option<u64>,
        /// Press Enter after the text (default true).
        #[serde(default)]
        enter: Option<bool>,
    },
}

impl Step {
    pub fn run(&self) -> &str {
        match self {
            Step::Run(s) => s,
            Step::Detailed { run, .. } => run,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub name: String,
    pub description: String,
    /// Shortcut, e.g. `cmd+shift+1`.
    pub key: String,
    /// `tab` (current page), `right`, `down` (split the focused terminal), `stack`
    /// (main + stack, the focused terminal stays main) or `page` (new page).
    pub open: String,
    /// Name of the new page when `open = "page"`.
    pub page: String,
    /// Name of the new terminal.
    pub title: String,
    pub cwd: String,
    pub steps: Vec<Step>,
}

impl Profile {
    /// Every text that may contain placeholders.
    pub fn texts(&self) -> Vec<&str> {
        let mut v = vec![self.page.as_str(), self.title.as_str(), self.cwd.as_str()];
        v.extend(self.steps.iter().map(Step::run));
        v
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct FontConfig {
    /// Family name looked up in the system font folders. Falls back to the built-in font.
    pub family: String,
    pub size: f32,
    pub line_height: f32,
}

impl Default for FontConfig {
    fn default() -> Self {
        Self { family: "JetBrains Mono, SF Mono, Menlo".into(), size: 13.5, line_height: 1.25 }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeConfig {
    pub name: String,
    /// Per-color overrides on top of the named theme (`background = "#101010"`).
    pub colors: BTreeMap<String, String>,
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self { name: "true-black".into(), colors: BTreeMap::new() }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ShellConfig {
    /// Empty = the user's login shell.
    pub program: String,
    pub args: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CursorStyleCfg {
    Block,
    Beam,
    Underline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OptionAsAlt {
    None,
    Left,
    Right,
    Both,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NewSessionCwd {
    /// Open new terminals in the directory of the focused one.
    Inherit,
    Home,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct TerminalConfig {
    pub scrollback: usize,
    pub cursor_style: CursorStyleCfg,
    pub cursor_blink: bool,
    pub option_as_alt: OptionAsAlt,
    pub copy_on_select: bool,
    /// Bytes sent for Shift+Enter. The default inserts a newline in Claude Code and other TUIs.
    pub shift_enter: String,
    pub new_session_cwd: NewSessionCwd,
    pub scroll_multiplier: f32,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            scrollback: 10_000,
            cursor_style: CursorStyleCfg::Block,
            cursor_blink: false,
            option_as_alt: OptionAsAlt::Left,
            copy_on_select: false,
            shift_enter: "\x1b\r".into(),
            new_session_cwd: NewSessionCwd::Inherit,
            scroll_multiplier: 3.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ImagePaste {
    /// Save the image to a file and paste its path (works with Claude Code, Codex, aider...).
    Path,
    /// Send a literal Ctrl+V and let the program read the clipboard itself.
    CtrlV,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ClipboardConfig {
    pub image_paste: ImagePaste,
    /// Delete pasted screenshots older than this many days.
    pub keep_pastes_days: u64,
}

impl Default for ClipboardConfig {
    fn default() -> Self {
        Self { image_paste: ImagePaste::Path, keep_pastes_days: 7 }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct PersistenceConfig {
    /// Save scrollback to disk so terminals come back even after a reboot.
    pub save_scrollback: bool,
    /// How many lines of history to keep per terminal on disk.
    pub scrollback_lines: usize,
    /// Seconds between scrollback snapshots.
    pub interval_secs: u64,
    /// After a restart, run `claude --resume <id>` in terminals where Claude Code was running.
    pub resume_claude: bool,
}

impl Default for PersistenceConfig {
    fn default() -> Self {
        Self { save_scrollback: true, scrollback_lines: 3000, interval_secs: 15, resume_claude: true }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModeCfg {
    Tabs,
    Grid,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub default_mode: ModeCfg,
    pub sidebar_width: f32,
    pub pane_gap: f32,
    pub padding: f32,
    pub pane_titles: bool,
    pub ui_font_size: f32,
    /// macOS notifications when Claude finishes or needs you while the window is in the background.
    pub notifications: bool,
    /// Interface font: "mono" (the terminal font) or "system".
    pub font: String,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            default_mode: ModeCfg::Tabs,
            sidebar_width: 230.0,
            pane_gap: 1.0,
            padding: 8.0,
            pane_titles: false,
            ui_font_size: 14.0,
            notifications: true,
            font: "mono".into(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct SecurityConfig {
    /// Allow programs to write the clipboard through OSC 52 (reading is never allowed).
    pub osc52_copy: bool,
    /// Ask before pasting text that contains a newline into a shell without bracketed paste.
    pub confirm_multiline_paste: bool,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self { osc52_copy: true, confirm_multiline_paste: true }
    }
}

pub const DEFAULT_CONFIG: &str = r##"# sennya configuration. Changes are picked up automatically.

[font]
# Comma separated list, the first family found in the system font folders wins.
family = "JetBrains Mono, SF Mono, Menlo"
size = 13.5
line_height = 1.25

[theme]
# true-black, sennya, tokyo-night, catppuccin-mocha, dracula, nord, gruvbox-dark,
# one-dark, solarized-dark, catppuccin-latte, github-light
name = "true-black"
# Override any color of the theme:
# [theme.colors]
# background = "#0e1621"
# foreground = "#e9eef3"
# cursor = "#2aabee"
# accent = "#2aabee"
# selection = "#2b5278"
# panel = "#17212b"     # sidebar and bars
# black = "#1b1e27"   # also: red green yellow blue magenta cyan white,
#                     # bright_black ... bright_white

[shell]
# program = "/bin/zsh"   # empty = your login shell
# args = ["-l"]

[env]
# EDITOR = "nvim"

[terminal]
scrollback = 10000
cursor_style = "block"      # block | beam | underline
cursor_blink = false
option_as_alt = "left"      # none | left | right | both
copy_on_select = false
new_session_cwd = "inherit" # inherit | home
scroll_multiplier = 3.0

[clipboard]
# What Cmd+V does when the clipboard holds an image (e.g. a screenshot):
#   "path"   - save it as PNG and paste the file path (Claude Code shows it as [Image #1])
#   "ctrl-v" - send Ctrl+V so the program grabs the clipboard itself
image_paste = "path"
keep_pastes_days = 7

[persistence]
save_scrollback = true
scrollback_lines = 3000
interval_secs = 15
resume_claude = true        # reopen Claude Code chats after a restart

[ui]
default_mode = "tabs"       # tabs | grid
sidebar_width = 230
pane_gap = 1                # hairline between panes
padding = 8                 # space between a pane's edge and its text
pane_titles = false         # a title bar above every pane in the grid
ui_font_size = 14
notifications = true        # notify when Claude finishes or needs you (window in background)
font = "mono"               # interface font: mono | system

[claude]
# Close Claude Code in terminals that are out of sight and idle at the prompt for this
# long and reopen the chat (`claude --resume`, same flags) when you look at them again.
# Each sleeping chat frees about 250 MB. 0 turns it off.
hibernate_after_mins = 30

[security]
osc52_copy = true
confirm_multiline_paste = true

[keys]
# Override any shortcut. "cmd" is Ctrl on Linux.
# new_terminal = "cmd+t"
# toggle_mode = "cmd+shift+g"

# Profiles: one shortcut opens a terminal and runs commands. Run them from the
# keyboard, from ⌘P, or with `sennya run <name>`. Placeholders: {uuid} {short}
# {words} {timestamp} {date} {time} {unix} {home} {cwd} {env:NAME} {prompt:Label}
#
# [[profile]]
# name = "prod-task"
# description = "SSH to prod and start a fresh task folder"
# key = "cmd+shift+1"
# open = "page"               # tab | right | down | page
# page = "prod {words}"
# title = "prod {short}"
# cwd = "~"
# steps = [
#   "ssh -t deploy@prod.example.com",
#   { wait_for = "$", run = "mkdir -p ~/tasks/{prompt:Task}-{timestamp} && cd ~/tasks/{prompt:Task}-{timestamp}" },
#   { wait_for = "$", run = "claude" },
# ]
#
# Servers with their own sennya daemon: terminals there survive network drops.
# Set one up with `sennya remote install HOST`, open it with `sennya remote HOST`.
# [[remote]]
# name = "dev"
# host = "dev"              # as you would type it after `ssh`
#
# [[profile]]
# name = "scratch"
# key = "cmd+shift+n"
# cwd = "~/scratch"
# steps = ["mkdir -p ~/scratch/{date}-{words} && cd $_"]
"##;

impl Config {
    /// Load the config, writing the documented default file on first run.
    pub fn load() -> (Config, Option<String>) {
        let path = paths::config_file();
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                let _ = fs::create_dir_all(paths::config_dir());
                let _ = fs::write(&path, DEFAULT_CONFIG);
                DEFAULT_CONFIG.to_string()
            }
            // Never replace a config we merely failed to read (bad encoding, permissions...).
            Err(err) => return (Config::default(), Some(format!("cannot read config.toml: {err}"))),
        };
        match toml::from_str::<Config>(&text) {
            Ok(cfg) => (cfg, None),
            Err(err) => (Config::default(), Some(format!("config.toml: {err}"))),
        }
    }

    pub fn profile(&self, name: &str) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.name.eq_ignore_ascii_case(name))
    }

    pub fn theme(&self) -> Theme {
        let mut theme = Theme::builtin(&self.theme.name).unwrap_or_else(|| Theme::builtin("true-black").unwrap());
        for (key, value) in &self.theme.colors {
            if let Some(rgb) = parse_hex(value) {
                theme.set(key, rgb);
            }
        }
        theme
    }
}

/// Set (or with `None` remove) `key = "value"` in `[section]` of the config file,
/// keeping every other line, comment and the order as they are.
pub fn set_config_value(section: &str, key: &str, value: Option<&str>) -> std::io::Result<()> {
    let path = paths::config_file();
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => DEFAULT_CONFIG.to_string(),
        Err(err) => return Err(err),
    };
    // `[keys]`, `[ keys ]` and `[keys]  # comment` all open the same table.
    let header_of = |t: &str| -> Option<String> {
        let t = t.split('#').next().unwrap_or_default().trim();
        (t.starts_with('[') && !t.starts_with("[[") && t.ends_with(']')).then(|| t[1..t.len() - 1].trim().to_string())
    };
    let is_key = |t: &str| t.strip_prefix(key).is_some_and(|rest| rest.trim_start().starts_with('='));
    let line = value.map(|v| format!("{key} = \"{}\"", v.replace('\\', "\\\\").replace('"', "\\\"")));
    let mut out: Vec<String> = Vec::new();
    let mut in_section = false;
    let mut seen_section = false;
    let mut done = false;
    for l in text.lines() {
        let t = l.trim();
        if t.starts_with('[') {
            if in_section && !done {
                // Insert before the comments that trail the section.
                let at = out.iter().rposition(|x| !x.trim().is_empty() && !x.trim().starts_with('#')).map(|i| i + 1).unwrap_or(out.len());
                if let Some(line) = &line {
                    out.insert(at, line.clone());
                }
                done = true;
            }
            in_section = header_of(t).as_deref() == Some(section);
            seen_section |= in_section;
        }
        if in_section && !done && is_key(t) {
            if let Some(line) = &line {
                out.push(line.clone());
            }
            done = true;
            continue;
        }
        out.push(l.to_string());
    }
    if !done {
        if let Some(line) = line {
            if !seen_section {
                out.push(String::new());
                out.push(format!("[{section}]"));
            }
            out.push(line);
        }
    }
    let new_text = out.join("\n") + "\n";
    if let Err(err) = toml::from_str::<toml::Table>(&new_text) {
        return Err(std::io::Error::other(format!("not changing config.toml, the result would not parse: {err}")));
    }
    fs::write(path, new_text)
}

pub type Rgb = [u8; 3];

pub fn parse_hex(s: &str) -> Option<Rgb> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(s, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub name: String,
    pub dark: bool,
    pub background: Rgb,
    pub foreground: Rgb,
    pub cursor: Rgb,
    pub selection: Rgb,
    pub accent: Rgb,
    /// Sidebar / bars. `None` = derived from the background.
    pub panel: Option<Rgb>,
    pub ansi: [Rgb; 16],
}

const ANSI_NAMES: [&str; 16] = [
    "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
    "bright_black", "bright_red", "bright_green", "bright_yellow", "bright_blue",
    "bright_magenta", "bright_cyan", "bright_white",
];

pub const THEME_NAMES: [&str; 11] = [
    "sennya", "true-black", "tokyo-night", "catppuccin-mocha", "dracula", "nord", "gruvbox-dark",
    "one-dark", "solarized-dark", "catppuccin-latte", "github-light",
];

impl Theme {
    fn set(&mut self, key: &str, rgb: Rgb) {
        match key {
            "background" => self.background = rgb,
            "foreground" => self.foreground = rgb,
            "cursor" => self.cursor = rgb,
            "selection" => self.selection = rgb,
            "accent" => self.accent = rgb,
            "panel" => self.panel = Some(rgb),
            other => {
                if let Some(i) = ANSI_NAMES.iter().position(|n| *n == other) {
                    self.ansi[i] = rgb;
                }
            }
        }
    }

    pub fn builtin(name: &str) -> Option<Theme> {
        // bg, fg, cursor, selection, accent, 16 ansi colors
        let (dark, spec): (bool, [&str; 21]) = match name {
            // Telegram night palette with Telegram's brand blue.
            "sennya" => (true, [
                "#0e1621", "#e9eef3", "#2aabee", "#2b5278", "#2aabee",
                "#1b1e27", "#f07178", "#a6da95", "#eed49f", "#82aaff", "#c792ea", "#5eead4", "#c8ccd6",
                "#4b5263", "#ff8b92", "#c3e88d", "#ffcb6b", "#9cc4ff", "#dab2ff", "#89f0e0", "#ffffff",
            ]),
            "true-black" => (true, [
                "#000000", "#e6e6e6", "#2aabee", "#1d3550", "#2aabee",
                "#000000", "#ff5f6d", "#5fd38d", "#f5c26b", "#3ea8ff", "#c38cff", "#4fd6d6", "#c7c7c7",
                "#5c5c5c", "#ff7a85", "#7ee2a5", "#ffd28a", "#6cbcff", "#d4a8ff", "#75e5e5", "#ffffff",
            ]),
            "tokyo-night" => (true, [
                "#1a1b26", "#c0caf5", "#c0caf5", "#33467c", "#7aa2f7",
                "#15161e", "#f7768e", "#9ece6a", "#e0af68", "#7aa2f7", "#bb9af7", "#7dcfff", "#a9b1d6",
                "#414868", "#f7768e", "#9ece6a", "#e0af68", "#7aa2f7", "#bb9af7", "#7dcfff", "#c0caf5",
            ]),
            "catppuccin-mocha" => (true, [
                "#1e1e2e", "#cdd6f4", "#f5e0dc", "#45475a", "#cba6f7",
                "#45475a", "#f38ba8", "#a6e3a1", "#f9e2af", "#89b4fa", "#f5c2e7", "#94e2d5", "#bac2de",
                "#585b70", "#f38ba8", "#a6e3a1", "#f9e2af", "#89b4fa", "#f5c2e7", "#94e2d5", "#a6adc8",
            ]),
            "dracula" => (true, [
                "#282a36", "#f8f8f2", "#f8f8f2", "#44475a", "#bd93f9",
                "#21222c", "#ff5555", "#50fa7b", "#f1fa8c", "#bd93f9", "#ff79c6", "#8be9fd", "#f8f8f2",
                "#6272a4", "#ff6e6e", "#69ff94", "#ffffa5", "#d6acff", "#ff92df", "#a4ffff", "#ffffff",
            ]),
            "nord" => (true, [
                "#2e3440", "#d8dee9", "#d8dee9", "#434c5e", "#88c0d0",
                "#3b4252", "#bf616a", "#a3be8c", "#ebcb8b", "#81a1c1", "#b48ead", "#88c0d0", "#e5e9f0",
                "#4c566a", "#bf616a", "#a3be8c", "#ebcb8b", "#81a1c1", "#b48ead", "#8fbcbb", "#eceff4",
            ]),
            "gruvbox-dark" => (true, [
                "#282828", "#ebdbb2", "#ebdbb2", "#504945", "#fabd2f",
                "#282828", "#cc241d", "#98971a", "#d79921", "#458588", "#b16286", "#689d6a", "#a89984",
                "#928374", "#fb4934", "#b8bb26", "#fabd2f", "#83a598", "#d3869b", "#8ec07c", "#ebdbb2",
            ]),
            "one-dark" => (true, [
                "#282c34", "#abb2bf", "#528bff", "#3e4451", "#61afef",
                "#282c34", "#e06c75", "#98c379", "#e5c07b", "#61afef", "#c678dd", "#56b6c2", "#abb2bf",
                "#5c6370", "#e06c75", "#98c379", "#e5c07b", "#61afef", "#c678dd", "#56b6c2", "#ffffff",
            ]),
            "solarized-dark" => (true, [
                "#002b36", "#839496", "#93a1a1", "#073642", "#268bd2",
                "#073642", "#dc322f", "#859900", "#b58900", "#268bd2", "#d33682", "#2aa198", "#eee8d5",
                "#002b36", "#cb4b16", "#586e75", "#657b83", "#839496", "#6c71c4", "#93a1a1", "#fdf6e3",
            ]),
            "catppuccin-latte" => (false, [
                "#eff1f5", "#4c4f69", "#dc8a78", "#bcc0cc", "#8839ef",
                "#5c5f77", "#d20f39", "#40a02b", "#df8e1d", "#1e66f5", "#ea76cb", "#179299", "#acb0be",
                "#6c6f85", "#d20f39", "#40a02b", "#df8e1d", "#1e66f5", "#ea76cb", "#179299", "#bcc0cc",
            ]),
            "github-light" => (false, [
                "#ffffff", "#24292f", "#0969da", "#b6d7ff", "#0969da",
                "#24292f", "#cf222e", "#116329", "#4d2d00", "#0969da", "#8250df", "#1b7c83", "#6e7781",
                "#57606a", "#a40e26", "#1a7f37", "#633c01", "#218bff", "#a475f9", "#3192aa", "#8c959f",
            ]),
            _ => return None,
        };
        let c = |i: usize| parse_hex(spec[i]).unwrap();
        let mut ansi = [[0u8; 3]; 16];
        for (i, slot) in ansi.iter_mut().enumerate() {
            *slot = c(5 + i);
        }
        Some(Theme {
            name: name.to_string(),
            dark,
            background: c(0),
            foreground: c(1),
            cursor: c(2),
            selection: c(3),
            accent: c(4),
            panel: match name {
                "sennya" => parse_hex("#17212b"),
                "true-black" => parse_hex("#000000"),
                _ => None,
            },
            ansi,
        })
    }

    /// xterm 256 color palette entry.
    pub fn indexed(&self, i: u8) -> Rgb {
        match i {
            0..=15 => self.ansi[i as usize],
            16..=231 => {
                let i = i - 16;
                let step = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
                [step(i / 36), step((i / 6) % 6), step(i % 6)]
            }
            _ => {
                let v = 8 + (i - 232) * 10;
                [v, v, v]
            }
        }
    }
}

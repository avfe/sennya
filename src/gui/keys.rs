//! Shortcuts and translation of keyboard / mouse input into terminal byte sequences.

use std::collections::BTreeMap;

use winit::event::KeyEvent;
use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;

use crate::config::TerminalConfig;
use crate::protocol::mode;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Action {
    NewTerminal,
    CloseTerminal,
    SplitRight,
    SplitDown,
    ToggleMode,
    NewPage,
    ClosePage,
    NextPage,
    PrevPage,
    NextTerminal,
    PrevTerminal,
    GotoPage(u8),
    ToggleSidebar,
    Find,
    GlobalSearch,
    QuickSwitch,
    Copy,
    Paste,
    SelectAll,
    ClearScrollback,
    FontBigger,
    FontSmaller,
    FontReset,
    ZoomPane,
    Rebalance,
    FocusLeft,
    FocusRight,
    FocusUp,
    FocusDown,
    OpenConfig,
    Rename,
    ScrollPageUp,
    ScrollPageDown,
    ScrollTop,
    ScrollBottom,
    Quit,
    ClaudeHistory,
    HidePane,
    NewWindow,
    FocusMode,
    /// Run `[[profile]]` number N from the config.
    Profile(u16),
}

impl Action {
    pub fn name(self) -> String {
        match self {
            Action::GotoPage(n) => format!("goto_page_{n}"),
            Action::Profile(n) => format!("profile_{n}"),
            other => {
                let dbg = format!("{other:?}");
                let mut out = String::new();
                for (i, c) in dbg.chars().enumerate() {
                    if c.is_uppercase() && i > 0 {
                        out.push('_');
                    }
                    out.push(c.to_ascii_lowercase());
                }
                out
            }
        }
    }

    /// A readable name: `new_terminal` -> "New terminal".
    pub fn title(self) -> String {
        match self {
            Action::GotoPage(n) => format!("Go to page {n}"),
            Action::Profile(n) => format!("Profile {n}"),
            other => {
                let name = other.name().replace('_', " ");
                let mut c = name.chars();
                c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
            }
        }
    }

    pub fn all() -> Vec<Action> {
        use Action::*;
        let mut v = vec![
            NewTerminal, CloseTerminal, SplitRight, SplitDown, ToggleMode, NewPage, ClosePage,
            NextPage, PrevPage, NextTerminal, PrevTerminal, ToggleSidebar, Find, GlobalSearch,
            QuickSwitch, Copy, Paste, SelectAll, ClearScrollback, FontBigger, FontSmaller,
            FontReset, ZoomPane, Rebalance, FocusLeft, FocusRight, FocusUp, FocusDown,
            OpenConfig, Rename, ScrollPageUp, ScrollPageDown, ScrollTop, ScrollBottom, Quit,
            ClaudeHistory, HidePane, NewWindow, FocusMode,
        ];
        v.extend((1..=9).map(GotoPage));
        v
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChordKey {
    Char(char),
    Code(KeyCode),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chord {
    pub cmd: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub key: ChordKey,
}

impl Chord {
    /// Parse `"cmd+shift+t"`. `cmd` is Super on macOS and Ctrl elsewhere.
    pub fn parse(s: &str) -> Option<Chord> {
        let mut chord = Chord { cmd: false, ctrl: false, alt: false, shift: false, key: ChordKey::Char(' ') };
        let lower = s.to_ascii_lowercase();
        let parts: Vec<&str> = if lower.ends_with("++") {
            let mut p: Vec<&str> = lower[..lower.len() - 2].split('+').filter(|p| !p.is_empty()).collect();
            p.push("+");
            p
        } else {
            lower.split('+').collect()
        };
        let (key, mods) = parts.split_last()?;
        for m in mods {
            match m.trim() {
                "cmd" | "command" | "super" | "meta" => {
                    if cfg!(target_os = "macos") {
                        chord.cmd = true
                    } else {
                        chord.ctrl = true
                    }
                }
                "ctrl" | "control" => chord.ctrl = true,
                "alt" | "opt" | "option" => chord.alt = true,
                "shift" => chord.shift = true,
                _ => return None,
            }
        }
        let key = key.trim();
        chord.key = match key {
            "enter" | "return" => ChordKey::Code(KeyCode::Enter),
            "tab" => ChordKey::Code(KeyCode::Tab),
            "space" => ChordKey::Char(' '),
            "backspace" => ChordKey::Code(KeyCode::Backspace),
            "delete" => ChordKey::Code(KeyCode::Delete),
            "esc" | "escape" => ChordKey::Code(KeyCode::Escape),
            "up" => ChordKey::Code(KeyCode::ArrowUp),
            "down" => ChordKey::Code(KeyCode::ArrowDown),
            "left" => ChordKey::Code(KeyCode::ArrowLeft),
            "right" => ChordKey::Code(KeyCode::ArrowRight),
            "home" => ChordKey::Code(KeyCode::Home),
            "end" => ChordKey::Code(KeyCode::End),
            "pageup" => ChordKey::Code(KeyCode::PageUp),
            "pagedown" => ChordKey::Code(KeyCode::PageDown),
            "plus" => ChordKey::Char('='),
            "minus" => ChordKey::Char('-'),
            k if k.starts_with('f') && k.len() > 1 && k[1..].parse::<u8>().is_ok() => {
                let n: u8 = k[1..].parse().ok()?;
                ChordKey::Code(match n {
                    1 => KeyCode::F1,
                    2 => KeyCode::F2,
                    3 => KeyCode::F3,
                    4 => KeyCode::F4,
                    5 => KeyCode::F5,
                    6 => KeyCode::F6,
                    7 => KeyCode::F7,
                    8 => KeyCode::F8,
                    9 => KeyCode::F9,
                    10 => KeyCode::F10,
                    11 => KeyCode::F11,
                    12 => KeyCode::F12,
                    _ => return None,
                })
            }
            k if k.chars().count() == 1 => ChordKey::Char(k.chars().next()?),
            _ => return None,
        };
        Some(chord)
    }
}

/// US-layout character printed on a physical key, used when the active layout is not
/// Latin (e.g. Russian) so that Cmd+C and Ctrl+C keep working.
pub fn physical_char(code: KeyCode) -> Option<char> {
    use KeyCode::*;
    Some(match code {
        KeyA => 'a', KeyB => 'b', KeyC => 'c', KeyD => 'd', KeyE => 'e', KeyF => 'f', KeyG => 'g',
        KeyH => 'h', KeyI => 'i', KeyJ => 'j', KeyK => 'k', KeyL => 'l', KeyM => 'm', KeyN => 'n',
        KeyO => 'o', KeyP => 'p', KeyQ => 'q', KeyR => 'r', KeyS => 's', KeyT => 't', KeyU => 'u',
        KeyV => 'v', KeyW => 'w', KeyX => 'x', KeyY => 'y', KeyZ => 'z',
        Digit0 => '0', Digit1 => '1', Digit2 => '2', Digit3 => '3', Digit4 => '4',
        Digit5 => '5', Digit6 => '6', Digit7 => '7', Digit8 => '8', Digit9 => '9',
        BracketLeft => '[', BracketRight => ']', Backslash => '\\', Semicolon => ';',
        Quote => '\'', Comma => ',', Period => '.', Slash => '/', Minus => '-', Equal => '=',
        Backquote => '`', Space => ' ',
        _ => return None,
    })
}

/// The ASCII character of a key regardless of layout and modifiers.
fn base_char(event: &KeyEvent) -> Option<char> {
    if let Key::Character(s) = event.key_without_modifiers() {
        let mut chars = s.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            if c.is_ascii() {
                return Some(c.to_ascii_lowercase());
            }
        }
    }
    match event.physical_key {
        PhysicalKey::Code(code) => physical_char(code),
        _ => None,
    }
}

pub struct Keymap {
    bindings: Vec<(Chord, Action)>,
}

fn default_bindings() -> Vec<(&'static str, Action)> {
    use Action::*;
    let mac = cfg!(target_os = "macos");
    let mut v = vec![
        (if mac { "cmd+t" } else { "ctrl+shift+t" }, NewTerminal),
        (if mac { "cmd+w" } else { "ctrl+shift+w" }, CloseTerminal),
        (if mac { "cmd+d" } else { "ctrl+shift+d" }, SplitRight),
        (if mac { "cmd+shift+d" } else { "ctrl+shift+alt+d" }, SplitDown),
        (if mac { "cmd+g" } else { "ctrl+shift+g" }, ToggleMode),
        (if mac { "cmd+n" } else { "ctrl+shift+n" }, NewPage),
        (if mac { "cmd+shift+w" } else { "ctrl+shift+alt+w" }, ClosePage),
        (if mac { "cmd+alt+]" } else { "ctrl+pagedown" }, NextPage),
        (if mac { "cmd+alt+[" } else { "ctrl+pageup" }, PrevPage),
        (if mac { "cmd+shift+]" } else { "ctrl+tab" }, NextTerminal),
        (if mac { "cmd+shift+[" } else { "ctrl+shift+tab" }, PrevTerminal),
        (if mac { "cmd+b" } else { "ctrl+shift+b" }, ToggleSidebar),
        (if mac { "cmd+f" } else { "ctrl+shift+f" }, Find),
        (if mac { "cmd+shift+f" } else { "ctrl+shift+alt+f" }, GlobalSearch),
        (if mac { "cmd+p" } else { "ctrl+shift+p" }, QuickSwitch),
        (if mac { "cmd+c" } else { "ctrl+shift+c" }, Copy),
        (if mac { "cmd+v" } else { "ctrl+shift+v" }, Paste),
        (if mac { "cmd+a" } else { "ctrl+shift+a" }, SelectAll),
        (if mac { "cmd+k" } else { "ctrl+shift+k" }, ClearScrollback),
        (if mac { "cmd+=" } else { "ctrl+=" }, FontBigger),
        (if mac { "cmd+shift+=" } else { "ctrl+shift+=" }, FontBigger),
        (if mac { "cmd+-" } else { "ctrl+-" }, FontSmaller),
        (if mac { "cmd+0" } else { "ctrl+0" }, FontReset),
        (if mac { "cmd+shift+enter" } else { "ctrl+shift+enter" }, ZoomPane),
        (if mac { "cmd+shift+r" } else { "ctrl+shift+r" }, Rebalance),
        (if mac { "cmd+alt+left" } else { "ctrl+alt+left" }, FocusLeft),
        (if mac { "cmd+alt+right" } else { "ctrl+alt+right" }, FocusRight),
        (if mac { "cmd+alt+up" } else { "ctrl+alt+up" }, FocusUp),
        (if mac { "cmd+alt+down" } else { "ctrl+alt+down" }, FocusDown),
        (if mac { "cmd+," } else { "ctrl+," }, OpenConfig),
        ("f2", Rename),
        ("shift+pageup", ScrollPageUp),
        ("shift+pagedown", ScrollPageDown),
        ("shift+home", ScrollTop),
        ("shift+end", ScrollBottom),
        (if mac { "cmd+q" } else { "ctrl+shift+q" }, Quit),
        (if mac { "cmd+shift+h" } else { "ctrl+shift+h" }, ClaudeHistory),
        (if mac { "cmd+shift+m" } else { "ctrl+shift+m" }, HidePane),
        (if mac { "cmd+shift+n" } else { "ctrl+shift+alt+n" }, NewWindow),
        (if mac { "cmd+ctrl+f" } else { "f11" }, FocusMode),
    ];
    if cfg!(windows) {
        // As in Windows Terminal: Ctrl+C copies a selection and is ^C otherwise.
        v.push(("ctrl+c", Copy));
        v.push(("ctrl+v", Paste));
    }
    let digits = ["1", "2", "3", "4", "5", "6", "7", "8", "9"];
    for (i, d) in digits.iter().enumerate() {
        let chord: &'static str = if mac {
            Box::leak(format!("cmd+{d}").into_boxed_str())
        } else {
            Box::leak(format!("alt+{d}").into_boxed_str())
        };
        v.push((chord, GotoPage(i as u8 + 1)));
    }
    v
}

impl Keymap {
    pub fn new(overrides: &BTreeMap<String, String>, profiles: &[crate::config::Profile]) -> (Keymap, Vec<String>) {
        let mut errors = Vec::new();
        let mut bindings: Vec<(Chord, Action)> = Vec::new();
        let overridden: BTreeMap<Action, &String> = Action::all()
            .into_iter()
            .filter_map(|a| overrides.get(&a.name()).map(|chord| (a, chord)))
            .collect();
        for name in overrides.keys() {
            if !Action::all().iter().any(|a| &a.name() == name) {
                errors.push(format!("unknown action in [keys]: {name}"));
            }
        }
        for (chord, action) in default_bindings() {
            if !overridden.contains_key(&action) {
                bindings.push((Chord::parse(chord).expect("valid default chord"), action));
            }
        }
        for (action, chord) in overridden {
            // Several chords are separated by spaces or ", "; a chord may itself end in the
            // comma key ("cmd+,").
            for part in chord.split_whitespace() {
                let part = if part.len() > 1 && part.ends_with(',') && !part.ends_with("+,") { &part[..part.len() - 1] } else { part };
                if part.eq_ignore_ascii_case("none") {
                    continue;
                }
                match Chord::parse(part) {
                    Some(c) => bindings.insert(0, (c, action)),
                    None => errors.push(format!("bad key chord for {}: {part}", action.name())),
                }
            }
        }
        for (i, profile) in profiles.iter().enumerate() {
            if profile.key.trim().is_empty() {
                continue;
            }
            match Chord::parse(profile.key.trim()) {
                Some(c) => bindings.insert(0, (c, Action::Profile(i as u16))),
                None => errors.push(format!("bad key for profile {}: {}", profile.name, profile.key)),
            }
        }
        (Keymap { bindings }, errors)
    }

    pub fn lookup(&self, event: &KeyEvent, mods: ModifiersState) -> Option<Action> {
        let code = match event.physical_key {
            PhysicalKey::Code(code) => Some(code),
            _ => None,
        };
        let ch = base_char(event);
        self.bindings.iter().find_map(|(chord, action)| {
            let mods_ok = chord.cmd == mods.super_key()
                && chord.ctrl == mods.control_key()
                && chord.alt == mods.alt_key()
                && chord.shift == mods.shift_key();
            let key_ok = match chord.key {
                ChordKey::Code(c) => code == Some(c),
                ChordKey::Char(c) => ch == Some(c),
            };
            (mods_ok && key_ok).then_some(*action)
        })
    }

    /// Every chord bound to `action`.
    pub fn chords(&self, action: Action) -> Vec<Chord> {
        self.bindings.iter().filter(|(_, a)| *a == action).map(|(c, _)| *c).collect()
    }

    /// Actions other than `except` that use `chord`.
    pub fn users(&self, chord: &Chord, except: Action) -> Vec<Action> {
        self.bindings.iter().filter(|(c, a)| c == chord && *a != except).map(|(_, a)| *a).collect()
    }

    /// Human readable shortcut for menus, e.g. `⌘T`.
    pub fn label(&self, action: Action) -> String {
        match self.bindings.iter().find(|(_, a)| *a == action) {
            Some((chord, _)) => chord.label(),
            None => String::new(),
        }
    }
}

impl Chord {
    /// The chord as written in the config: `cmd+shift+t`.
    pub fn to_config(&self) -> String {
        let mac = cfg!(target_os = "macos");
        let mut parts: Vec<String> = Vec::new();
        if self.cmd {
            parts.push("cmd".into());
        }
        if self.ctrl {
            parts.push(if mac { "ctrl" } else { "cmd" }.into());
        }
        if self.alt {
            parts.push("alt".into());
        }
        if self.shift {
            parts.push("shift".into());
        }
        parts.push(match self.key {
            ChordKey::Char(' ') => "space".into(),
            ChordKey::Char('+') => "plus".into(),
            ChordKey::Char(c) => c.to_string(),
            ChordKey::Code(code) => code_name(code).to_string(),
        });
        parts.join("+")
    }

    /// The chord a key press makes, or `None` for a lone modifier.
    pub fn from_event(event: &KeyEvent, mods: ModifiersState) -> Option<Chord> {
        let PhysicalKey::Code(code) = event.physical_key else { return None };
        let key = match code {
            KeyCode::ShiftLeft | KeyCode::ShiftRight | KeyCode::ControlLeft | KeyCode::ControlRight
            | KeyCode::AltLeft | KeyCode::AltRight | KeyCode::SuperLeft | KeyCode::SuperRight
            | KeyCode::CapsLock | KeyCode::Fn => return None,
            // The same key notion as `Keymap::lookup` (the layout's letter first), so a
            // recorded shortcut fires on the key that was pressed.
            code => match base_char(event) {
                Some(c) => ChordKey::Char(c),
                None if code_name(code) != "?" => ChordKey::Code(code),
                None => return None,
            },
        };
        let mac = cfg!(target_os = "macos");
        Some(Chord {
            cmd: if mac { mods.super_key() } else { false },
            ctrl: if mac { mods.control_key() } else { mods.control_key() || mods.super_key() },
            alt: mods.alt_key(),
            shift: mods.shift_key(),
            key,
        })
    }

    pub fn label(&self) -> String {
        let chord = self;
        let mac = cfg!(target_os = "macos");
        let mut parts: Vec<String> = Vec::new();
        if chord.ctrl {
            parts.push(if mac { "⌃" } else { "Ctrl" }.into());
        }
        if chord.alt {
            parts.push(if mac { "⌥" } else { "Alt" }.into());
        }
        if chord.shift {
            parts.push(if mac { "⇧" } else { "Shift" }.into());
        }
        if chord.cmd {
            parts.push("⌘".into());
        }
        parts.push(match chord.key {
            ChordKey::Char(' ') => "Space".into(),
            ChordKey::Char(c) => c.to_ascii_uppercase().to_string(),
            ChordKey::Code(code) => match code {
                KeyCode::Enter => "↩",
                KeyCode::Tab => "Tab",
                KeyCode::ArrowLeft => "←",
                KeyCode::ArrowRight => "→",
                KeyCode::ArrowUp => "↑",
                KeyCode::ArrowDown => "↓",
                KeyCode::F1 => "F1",
                KeyCode::F2 => "F2",
                KeyCode::F3 => "F3",
                KeyCode::F4 => "F4",
                KeyCode::F5 => "F5",
                KeyCode::F6 => "F6",
                KeyCode::F7 => "F7",
                KeyCode::F8 => "F8",
                KeyCode::F9 => "F9",
                KeyCode::F10 => "F10",
                KeyCode::F11 => "F11",
                KeyCode::F12 => "F12",
                KeyCode::PageUp => "PgUp",
                KeyCode::PageDown => "PgDn",
                KeyCode::Home => "Home",
                KeyCode::End => "End",
                KeyCode::Backspace => "⌫",
                KeyCode::Delete => "⌦",
                KeyCode::Escape => "Esc",
                _ => "?",
            }
            .into(),
        });
        // A thin space keeps the symbols from touching each other.
        parts.join(if mac { "\u{2009}" } else { "+" })
    }
}

fn code_name(code: KeyCode) -> &'static str {
    use KeyCode::*;
    match code {
        Enter => "enter",
        Tab => "tab",
        Backspace => "backspace",
        Delete => "delete",
        Escape => "escape",
        ArrowUp => "up",
        ArrowDown => "down",
        ArrowLeft => "left",
        ArrowRight => "right",
        Home => "home",
        End => "end",
        PageUp => "pageup",
        PageDown => "pagedown",
        F1 => "f1",
        F2 => "f2",
        F3 => "f3",
        F4 => "f4",
        F5 => "f5",
        F6 => "f6",
        F7 => "f7",
        F8 => "f8",
        F9 => "f9",
        F10 => "f10",
        F11 => "f11",
        F12 => "f12",
        _ => "?",
    }
}

fn ctrl_byte(c: char) -> Option<u8> {
    Some(match c {
        'a'..='z' => c as u8 - b'a' + 1,
        '@' | ' ' | '2' => 0,
        '[' | '3' => 27,
        '\\' | '4' => 28,
        ']' | '5' => 29,
        '^' | '6' => 30,
        '_' | '-' | '7' | '/' => 31,
        '8' | '?' => 127,
        _ => return None,
    })
}

fn csi_mod(m: u8, letter: char, app_cursor: bool) -> Vec<u8> {
    if m > 1 {
        format!("\x1b[1;{m}{letter}").into_bytes()
    } else if app_cursor {
        format!("\x1bO{letter}").into_bytes()
    } else {
        format!("\x1b[{letter}").into_bytes()
    }
}

fn tilde(code: u8, m: u8) -> Vec<u8> {
    if m > 1 {
        format!("\x1b[{code};{m}~").into_bytes()
    } else {
        format!("\x1b[{code}~").into_bytes()
    }
}

/// Bytes to send to the PTY for a key press, or `None` if the key produces nothing.
pub fn encode_key(event: &KeyEvent, mods: ModifiersState, term_mode: u32, cfg: &TerminalConfig) -> Option<Vec<u8>> {
    let shift = mods.shift_key();
    let ctrl = mods.control_key();
    let alt = mods.alt_key();
    let sup = mods.super_key();
    let app_cursor = term_mode & mode::APP_CURSOR != 0;
    let m = 1 + shift as u8 + 2 * alt as u8 + 4 * ctrl as u8;

    if let Key::Named(named) = &event.logical_key {
        // macOS "natural text editing": Cmd/Option + arrows and backspace.
        if cfg!(target_os = "macos") {
            match (named, sup, alt, ctrl, shift) {
                (NamedKey::ArrowLeft, true, false, false, false) => return Some(vec![0x01]),
                (NamedKey::ArrowRight, true, false, false, false) => return Some(vec![0x05]),
                (NamedKey::Backspace, true, false, false, false) => return Some(vec![0x15]),
                (NamedKey::ArrowLeft, false, true, false, false) => return Some(b"\x1bb".to_vec()),
                (NamedKey::ArrowRight, false, true, false, false) => return Some(b"\x1bf".to_vec()),
                _ => {}
            }
        }
        if sup {
            return None;
        }
        let bytes = match named {
            NamedKey::Enter => {
                if shift && !ctrl && !alt {
                    cfg.shift_enter.clone().into_bytes()
                } else if alt {
                    b"\x1b\r".to_vec()
                } else {
                    b"\r".to_vec()
                }
            }
            NamedKey::Backspace => {
                if ctrl {
                    vec![0x08]
                } else if alt {
                    b"\x1b\x7f".to_vec()
                } else {
                    vec![0x7f]
                }
            }
            NamedKey::Tab => {
                if shift {
                    b"\x1b[Z".to_vec()
                } else {
                    b"\t".to_vec()
                }
            }
            NamedKey::Escape => vec![0x1b],
            NamedKey::Space => {
                if ctrl {
                    vec![0]
                } else if alt {
                    b"\x1b ".to_vec()
                } else {
                    b" ".to_vec()
                }
            }
            NamedKey::ArrowUp => csi_mod(m, 'A', app_cursor),
            NamedKey::ArrowDown => csi_mod(m, 'B', app_cursor),
            NamedKey::ArrowRight => csi_mod(m, 'C', app_cursor),
            NamedKey::ArrowLeft => csi_mod(m, 'D', app_cursor),
            NamedKey::Home => csi_mod(m, 'H', app_cursor),
            NamedKey::End => csi_mod(m, 'F', app_cursor),
            NamedKey::Insert => tilde(2, m),
            NamedKey::Delete => tilde(3, m),
            NamedKey::PageUp => tilde(5, m),
            NamedKey::PageDown => tilde(6, m),
            NamedKey::F1 => csi_mod(m, 'P', true),
            NamedKey::F2 => csi_mod(m, 'Q', true),
            NamedKey::F3 => csi_mod(m, 'R', true),
            NamedKey::F4 => csi_mod(m, 'S', true),
            NamedKey::F5 => tilde(15, m),
            NamedKey::F6 => tilde(17, m),
            NamedKey::F7 => tilde(18, m),
            NamedKey::F8 => tilde(19, m),
            NamedKey::F9 => tilde(20, m),
            NamedKey::F10 => tilde(21, m),
            NamedKey::F11 => tilde(23, m),
            NamedKey::F12 => tilde(24, m),
            _ => return None,
        };
        return Some(bytes);
    }

    if sup {
        return None;
    }
    if let Key::Dead(_) = event.logical_key {
        return None;
    }

    let base = base_char(event);
    if ctrl {
        if let Some(b) = base.and_then(|c| ctrl_byte(if shift && c == '2' { '@' } else { c })) {
            return Some(if alt { vec![0x1b, b] } else { vec![b] });
        }
    }

    let text = event.text.as_ref()?.to_string();
    if text.is_empty() {
        return None;
    }
    if alt {
        // With "option as alt" winit hands us the plain character; prefix ESC (meta).
        // Otherwise Option produced a special character which we send as typed.
        let plain = event.key_without_modifiers();
        let is_plain = match plain {
            Key::Character(s) => s.eq_ignore_ascii_case(&text),
            _ => false,
        };
        if is_plain || !cfg!(target_os = "macos") {
            let mut out = vec![0x1b];
            out.extend_from_slice(text.as_bytes());
            return Some(out);
        }
    }
    Some(text.into_bytes())
}

/// xterm mouse report. `button`: 0 left, 1 middle, 2 right, 64/65 wheel up/down.
pub fn mouse_report(button: u8, pressed: bool, motion: bool, col: u16, row: u16, mods: ModifiersState, term_mode: u32) -> Option<Vec<u8>> {
    let mut code = button;
    if mods.shift_key() {
        code += 4;
    }
    if mods.alt_key() {
        code += 8;
    }
    if mods.control_key() {
        code += 16;
    }
    if motion {
        code += 32;
    }
    if term_mode & mode::SGR_MOUSE != 0 {
        let suffix = if pressed { 'M' } else { 'm' };
        return Some(format!("\x1b[<{code};{};{}{suffix}", col + 1, row + 1).into_bytes());
    }
    if !pressed {
        code = (code & !3) | 3;
    }
    let (x, y) = (col as u32 + 33, row as u32 + 33);
    if term_mode & mode::UTF8_MOUSE != 0 {
        let mut s = String::from("\x1b[M");
        s.push(char::from_u32(32 + code as u32)?);
        s.push(char::from_u32(x)?);
        s.push(char::from_u32(y)?);
        return Some(s.into_bytes());
    }
    if x > 255 || y > 255 {
        return None;
    }
    Some(vec![0x1b, b'[', b'M', 32 + code, x as u8, y as u8])
}

/// Make pasted text safe: bracketed paste must not be terminable from inside the paste.
pub fn paste_bytes(text: &str, term_mode: u32) -> Vec<u8> {
    if term_mode & mode::BRACKETED_PASTE != 0 {
        let clean: String = text.chars().filter(|c| *c != '\x1b' && *c != '\u{9b}').collect();
        let mut out = b"\x1b[200~".to_vec();
        out.extend_from_slice(clean.replace("\r\n", "\n").as_bytes());
        out.extend_from_slice(b"\x1b[201~");
        out
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

/// Escape a path the way Terminal.app/iTerm2 do for drag & drop.
pub fn shell_escape(path: &str) -> String {
    if cfg!(windows) {
        return crate::template::local_word(path);
    }
    let mut out = String::with_capacity(path.len() + 8);
    for c in path.chars() {
        if " \t\\'\"()[]{}<>|&;$`!*?#~=%".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chords() {
        let c = Chord::parse("cmd+shift+t").unwrap();
        assert!(c.shift);
        assert_eq!(c.key, ChordKey::Char('t'));
        assert_eq!(Chord::parse("ctrl+tab").unwrap().key, ChordKey::Code(KeyCode::Tab));
        assert!(Chord::parse("hyper+x").is_none());
        let (km, errs) = Keymap::new(&BTreeMap::new(), &[]);
        assert!(errs.is_empty());
        assert!(!km.label(Action::NewTerminal).is_empty());
    }

    #[test]
    fn chords_round_trip_through_the_config() {
        for text in ["cmd+shift+]", "cmd+alt+left", "ctrl+tab", "f2", "cmd+shift+enter", "cmd+=", "cmd+,"] {
            let chord = Chord::parse(text).unwrap();
            assert_eq!(Chord::parse(&chord.to_config()), Some(chord), "{text}");
        }
        let mut over = BTreeMap::new();
        over.insert("open_config".to_string(), "cmd+shift+,".to_string());
        over.insert("find".to_string(), "cmd+f, cmd+shift+;".to_string());
        let (km, errs) = Keymap::new(&over, &[]);
        assert!(errs.is_empty(), "{errs:?}");
        assert_eq!(km.chords(Action::OpenConfig), vec![Chord::parse("cmd+shift+,").unwrap()]);
        assert_eq!(km.chords(Action::Find).len(), 2);
        let next = km.chords(Action::NextTerminal);
        assert_eq!(next.len(), 1);
        assert!(km.users(&next[0], Action::NextTerminal).is_empty());
    }

    #[test]
    fn paste_is_sanitized() {
        let bytes = paste_bytes("evil\x1b[201~rm -rf /\n", mode::BRACKETED_PASTE);
        let s = String::from_utf8(bytes).unwrap();
        assert_eq!(s.matches("\x1b[201~").count(), 1);
        assert_eq!(paste_bytes("a\nb", 0), b"a\rb");
    }

    #[test]
    fn mouse() {
        let m = ModifiersState::empty();
        assert_eq!(mouse_report(0, true, false, 4, 2, m, mode::SGR_MOUSE).unwrap(), b"\x1b[<0;5;3M");
        assert_eq!(mouse_report(0, false, false, 0, 0, m, 0).unwrap(), vec![0x1b, b'[', b'M', 35, 33, 33]);
        assert_eq!(shell_escape("/a b/c(1).png"), "/a\\ b/c\\(1\\).png");
    }
}

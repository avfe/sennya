//! Settings → Profiles: make, change and run profiles without editing config.toml by hand,
//! record one from the commands typed in a terminal, or describe one to Claude.

use super::*;
use crate::config::{Profile, Step};
use crate::protocol::{Frame, Line, SpawnSpec};

/// Endings of the usual shell prompts, for "wait for a prompt".
const PROMPT_ENDS: &str = "$|%|#|>";
const OPEN_CHOICES: [(&str, &str); 5] =
    [("right", "Split right"), ("down", "Split down"), ("stack", "Stack on the right"), ("tab", "Tab on this page"), ("page", "New page")];

pub(super) struct ProfileEditor {
    /// The saved name of the profile being edited; `None` while it is new.
    original: Option<String>,
    draft: Draft,
    /// The draft as last saved, to notice unsaved changes.
    saved: Draft,
    error: Option<String>,
    confirm_delete: bool,
    /// A click that would drop unsaved changes was answered with a warning; the next goes ahead.
    discard_armed: bool,
    focus_name: bool,
}

impl ProfileEditor {
    fn new(original: Option<String>, draft: Draft) -> Self {
        let saved = if original.is_some() { draft.clone() } else { Draft::default() };
        ProfileEditor { focus_name: original.is_none(), original, saved, draft, error: None, confirm_delete: false, discard_armed: false }
    }

    fn dirty(&self) -> bool {
        self.draft != self.saved
    }
}

#[derive(Clone, Default, PartialEq)]
struct Draft {
    name: String,
    description: String,
    key: String,
    open: String,
    page: String,
    title: String,
    cwd: String,
    steps: Vec<StepDraft>,
}

impl Draft {
    fn from_profile(p: &Profile) -> Self {
        Draft {
            name: p.name.clone(),
            description: p.description.clone(),
            key: p.key.clone(),
            open: if p.open.is_empty() { "tab".into() } else { p.open.clone() },
            page: p.page.clone(),
            title: p.title.clone(),
            cwd: p.cwd.clone(),
            steps: p.steps.iter().map(StepDraft::from_step).collect(),
        }
    }

    fn to_profile(&self) -> Profile {
        Profile {
            name: self.name.trim().into(),
            description: self.description.trim().into(),
            key: self.key.trim().into(),
            open: self.open.clone(),
            page: if self.open == "page" { self.page.trim().into() } else { String::new() },
            title: self.title.trim().into(),
            cwd: self.cwd.trim().into(),
            steps: self.steps.iter().filter(|s| !s.run.trim().is_empty()).map(StepDraft::to_step).collect(),
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Wait {
    Nothing,
    Prompt,
    Text,
}

#[derive(Clone, PartialEq)]
pub(super) struct StepDraft {
    run: String,
    wait: Wait,
    /// Prompt endings for `Wait::Prompt`, the text for `Wait::Text`.
    value: String,
    enter: bool,
    delay_ms: u64,
    timeout_ms: Option<u64>,
}

impl StepDraft {
    fn new(run: String) -> Self {
        StepDraft { run, wait: Wait::Nothing, value: String::new(), enter: true, delay_ms: 0, timeout_ms: None }
    }

    fn from_step(step: &Step) -> Self {
        match step {
            Step::Run(run) => Self::new(run.clone()),
            Step::Detailed { run, wait_for, wait_text, delay_ms, timeout_ms, enter } => {
                let (wait, value) = match (wait_for, wait_text) {
                    (Some(w), _) if !w.is_empty() => (Wait::Prompt, w.clone()),
                    (_, Some(t)) if !t.is_empty() => (Wait::Text, t.clone()),
                    _ => (Wait::Nothing, String::new()),
                };
                StepDraft { run: run.clone(), wait, value, enter: enter.unwrap_or(true), delay_ms: *delay_ms, timeout_ms: *timeout_ms }
            }
        }
    }

    fn to_step(&self) -> Step {
        let value = self.value.trim();
        let (wait_for, wait_text) = match self.wait {
            Wait::Prompt => (Some(if value.is_empty() { PROMPT_ENDS.into() } else { value.to_string() }), None),
            Wait::Text if !value.is_empty() => (None, Some(value.to_string())),
            _ => (None, None),
        };
        if wait_for.is_none() && wait_text.is_none() && self.enter && self.delay_ms == 0 && self.timeout_ms.is_none() {
            return Step::Run(self.run.clone());
        }
        Step::Detailed {
            run: self.run.clone(),
            wait_for,
            wait_text,
            delay_ms: self.delay_ms,
            timeout_ms: self.timeout_ms,
            enter: (!self.enter).then_some(false),
        }
    }
}

/// Commands typed in one terminal, written down as the steps of a new profile.
pub(super) struct Recording {
    sid: SessionId,
    cwd: String,
    steps: Vec<StepDraft>,
    /// What stands in front of the command being typed (`~/x % `), read at its first key.
    prompt: Option<String>,
}

enum EditorAction {
    Select(Option<usize>),
    Save,
    SaveAndRun,
    Delete,
    Record,
    AskClaude,
}

impl App {
    /// Open the editor on profile `index`, or on a new profile.
    pub(super) fn open_profile_editor(&mut self, index: Option<usize>) {
        self.profile_editor = Some(match index.and_then(|i| self.config.profiles.get(i)) {
            Some(p) => ProfileEditor::new(Some(p.name.clone()), Draft::from_profile(p)),
            None => ProfileEditor::new(None, Draft { open: "right".into(), steps: vec![StepDraft::new(String::new())], ..Default::default() }),
        });
    }

    fn save_draft(&mut self, ed: &mut ProfileEditor) -> bool {
        let p = ed.draft.to_profile();
        let taken = ed.original.as_deref() != Some(p.name.as_str()) && self.config.profiles.iter().any(|x| x.name == p.name);
        ed.error = if p.name.is_empty() {
            Some("Give the profile a name.".into())
        } else if taken {
            Some(format!("There is already a profile named {}.", p.name))
        } else if p.steps.is_empty() {
            Some("Add at least one command.".into())
        } else {
            crate::config::save_profile(ed.original.as_deref(), Some(&p)).err().map(|e| e.to_string())
        };
        if ed.error.is_some() {
            return false;
        }
        self.reload_config();
        ed.original = Some(p.name.clone());
        ed.draft = Draft::from_profile(&p);
        ed.saved = ed.draft.clone();
        true
    }

    pub(super) fn profile_editor_overlay(&mut self, ctx: &egui::Context) {
        let Some(mut ed) = self.profile_editor.take() else { return };
        let pal = self.pal.clone();
        let names: Vec<String> = self.config.profiles.iter().map(|p| p.name.clone()).collect();
        let mut open = true;
        let mut action: Option<EditorAction> = None;
        let modal = self.modal("profile-editor").show(ctx, |ui| {
            ui.set_width(820.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Profiles").size(17.0).strong());
                ui.label(RichText::new("one shortcut opens a terminal and types your commands").color(pal.dim));
            });
            ui.add_space(12.0);
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(180.0);
                    for (i, name) in names.iter().enumerate() {
                        let current = ed.original.as_deref() == Some(name.as_str());
                        if ui.selectable_label(current, name).clicked() && !current {
                            action = Some(EditorAction::Select(Some(i)));
                        }
                    }
                    if ui.selectable_label(ed.original.is_none(), "+ New profile").clicked() && ed.original.is_some() {
                        action = Some(EditorAction::Select(None));
                    }
                    ui.add_space(16.0);
                    let record = ui.button(RichText::new("● Record from a terminal").color(pal.danger));
                    if record.on_hover_text("Type the commands in the focused terminal as usual; sennya writes them down as steps").clicked() {
                        action = Some(EditorAction::Record);
                    }
                    let ask = ui.button("✳ Describe it to Claude…");
                    if ask.on_hover_text("Opens Claude Code next to you; it writes the profile into the config").clicked() {
                        action = Some(EditorAction::AskClaude);
                    }
                });
                ui.separator();
                ui.vertical(|ui| profile_form(ui, &mut ed, &pal));
            });
            ui.add_space(8.0);
            if let Some(err) = &ed.error {
                ui.label(RichText::new(err).color(pal.danger));
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ed.original.is_some() {
                    if ed.confirm_delete {
                        ui.label("Delete this profile?");
                        if ui.button(RichText::new("Delete").color(pal.danger)).clicked() {
                            action = Some(EditorAction::Delete);
                        }
                        if ui.button("Keep it").clicked() {
                            ed.confirm_delete = false;
                        }
                    } else if ui.button(RichText::new("Delete").color(pal.danger)).clicked() {
                        ed.confirm_delete = true;
                    }
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let save = egui::Button::new(RichText::new("Save").strong().color(Color32::WHITE)).fill(pal.accent).corner_radius(8).min_size(vec2(90.0, 32.0));
                    if ui.add(save).clicked() {
                        action = Some(EditorAction::Save);
                    }
                    if ui.add(egui::Button::new("Save and run").corner_radius(8).min_size(vec2(110.0, 32.0))).clicked() {
                        action = Some(EditorAction::SaveAndRun);
                    }
                    if ui.add(egui::Button::new("Close").corner_radius(8).min_size(vec2(80.0, 32.0))).clicked() {
                        open = false;
                    }
                });
            });
        });
        // Escape or a click outside never drops unsaved work without a warning.
        if (modal.should_close() || !open) && ed.dirty() && !ed.discard_armed {
            open = true;
            ed.discard_armed = true;
            ed.error = Some("You have unsaved changes: save them, or close again to drop them.".into());
        } else if modal.should_close() {
            open = false;
        }
        match action {
            Some(EditorAction::Select(index)) => {
                if ed.dirty() && !ed.discard_armed {
                    ed.discard_armed = true;
                    ed.error = Some("You have unsaved changes: save them, or click again to drop them.".into());
                } else {
                    self.open_profile_editor(index);
                    return;
                }
            }
            Some(EditorAction::Save) => {
                self.save_draft(&mut ed);
            }
            Some(EditorAction::SaveAndRun) => {
                if self.save_draft(&mut ed) {
                    if let Some(i) = self.config.profiles.iter().position(|p| Some(&p.name) == ed.original.as_ref()) {
                        self.start_profile(i);
                    }
                    open = false;
                }
            }
            Some(EditorAction::Delete) => match crate::config::save_profile(ed.original.as_deref(), None) {
                Ok(()) => {
                    self.reload_config();
                    self.open_profile_editor(if self.config.profiles.is_empty() { None } else { Some(0) });
                    return;
                }
                Err(err) => ed.error = Some(err.to_string()),
            },
            Some(EditorAction::Record) => match self.ws.focused() {
                Some(sid) => {
                    let cwd = self.sessions.get(&sid).map(|s| crate::paths::tilde(&s.info.cwd)).unwrap_or_default();
                    self.recording = Some(Recording { sid, cwd, steps: Vec::new(), prompt: None });
                    open = false;
                }
                None => ed.error = Some("Open a terminal to record in first.".into()),
            },
            Some(EditorAction::AskClaude) => {
                self.ask_claude_for_profile();
                open = false;
            }
            None => {}
        }
        if open {
            self.profile_editor = Some(ed);
        }
    }

    fn ask_claude_for_profile(&mut self) {
        let spec = SpawnSpec {
            open: "right".into(),
            near: self.ws.focused(),
            title: Some("✳ new profile".into()),
            run: vec!["claude".into()],
            prefill: Some("Use the sennya skill to add a profile to my sennya config: ".into()),
            prefill_after: Some("@claude-ready".into()),
            ..Default::default()
        };
        let req = self.req();
        self.send(ClientMsg::Spawn { req, spec });
    }

    /// Called with every input for a terminal: Enter in the terminal being recorded writes
    /// the command on its cursor line down as a step.
    pub(super) fn record_input(&mut self, sid: SessionId, bytes: &[u8]) {
        let Some(frame) = self.sessions.get(&sid).and_then(|s| s.frame.as_deref()) else { return };
        let Some(rec) = self.recording.as_mut().filter(|r| r.sid == sid) else { return };
        match bytes {
            b"\r" => {
                let prompt = rec.prompt.take().unwrap_or_default();
                let command = strip_prompt(&cursor_line(frame), &prompt);
                // Nothing on the line: an empty Enter, or a password the terminal did not echo.
                if command.is_empty() {
                    return;
                }
                let mut step = StepDraft::new(command);
                if !rec.steps.is_empty() {
                    step.wait = Wait::Prompt;
                    step.value = prompt_end(&prompt);
                }
                rec.steps.push(step);
            }
            // Ctrl+C: the shell starts over on a new line.
            [3] => rec.prompt = None,
            _ => {
                if rec.prompt.is_none() {
                    rec.prompt = Some(before_cursor(frame));
                }
            }
        }
    }

    pub(super) fn recording_overlay(&mut self, ctx: &egui::Context) {
        let Some(rec) = &self.recording else { return };
        let pal = self.pal.clone();
        let title = self.title_of(rec.sid);
        let count = rec.steps.len();
        let last = rec.steps.last().map(|s| s.run.clone());
        let mut stop: Option<bool> = None;
        egui::Area::new(Id::new("recording"))
            .anchor(Align2::CENTER_BOTTOM, vec2(0.0, -18.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(mix(pal.panel, pal.fg, 0.08))
                    .stroke(Stroke::new(1.0, pal.danger))
                    .corner_radius(10)
                    .inner_margin(egui::Margin::symmetric(14, 8))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("●").color(pal.danger));
                            let what = if count == 1 { "1 command".to_string() } else { format!("{count} commands") };
                            ui.label(format!("Recording “{title}”: {what}"));
                            if let Some(last) = last {
                                ui.label(RichText::new(last).monospace().color(pal.dim));
                            }
                            if ui.button("Stop and edit").clicked() {
                                stop = Some(true);
                            }
                            if ui.button("Cancel").clicked() {
                                stop = Some(false);
                            }
                        });
                    });
            });
        let Some(keep) = stop else { return };
        let Some(rec) = self.recording.take() else { return };
        if keep {
            let mut steps = rec.steps;
            if steps.is_empty() {
                steps.push(StepDraft::new(String::new()));
            }
            let draft = Draft { open: "right".into(), cwd: rec.cwd, steps, ..Default::default() };
            self.profile_editor = Some(ProfileEditor::new(None, draft));
        }
    }
}

fn profile_form(ui: &mut Ui, ed: &mut ProfileEditor, pal: &Palette) {
    let focus_name = std::mem::take(&mut ed.focus_name);
    let d = &mut ed.draft;
    let field = |ui: &mut Ui, text: &mut String, hint: &str, width: f32| ui.add(egui::TextEdit::singleline(text).desired_width(width).hint_text(hint));
    egui::Grid::new("profile-form").num_columns(2).spacing(vec2(12.0, 8.0)).show(ui, |ui| {
        ui.label("Name");
        let name = field(ui, &mut d.name, "dev-task", 420.0);
        if focus_name {
            name.request_focus();
        }
        ui.end_row();
        ui.label("Description");
        field(ui, &mut d.description, "ssh dev, a fresh folder, Claude with the task", 420.0);
        ui.end_row();
        ui.label("Shortcut");
        field(ui, &mut d.key, "cmd+shift+t", 160.0);
        ui.end_row();
        ui.label("Opens in");
        let current = OPEN_CHOICES.iter().find(|(v, _)| *v == d.open).map(|(_, l)| *l).unwrap_or("Tab on this page");
        egui::ComboBox::from_id_salt("profile-open").selected_text(current).show_ui(ui, |ui| {
            for (value, label) in OPEN_CHOICES {
                ui.selectable_value(&mut d.open, value.to_string(), label);
            }
        });
        ui.end_row();
        if d.open == "page" {
            ui.label("Page name");
            field(ui, &mut d.page, "dev {words}", 260.0);
            ui.end_row();
        }
        ui.label("Terminal title");
        field(ui, &mut d.title, "task {short}", 260.0);
        ui.end_row();
        ui.label("Folder");
        field(ui, &mut d.cwd, "the focused terminal's folder", 260.0);
        ui.end_row();
    });
    ui.add_space(10.0);
    ui.label(RichText::new("Commands").strong());
    ui.add_space(4.0);
    let (mut remove, mut swap) = (None, None);
    let count = d.steps.len();
    egui::ScrollArea::vertical().max_height(250.0).auto_shrink([false, true]).show(ui, |ui| {
        for (i, step) in d.steps.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{:>2}", i + 1)).monospace().color(pal.dim));
                let hint = if i == 0 { "ssh -t dev" } else { "claude {prompt:Task|q}" };
                ui.add(egui::TextEdit::singleline(&mut step.run).desired_width(270.0).hint_text(hint).font(egui::TextStyle::Monospace));
                let when = match step.wait {
                    Wait::Nothing if i == 0 => "when the shell is ready",
                    Wait::Nothing => "right away",
                    Wait::Prompt => "after a prompt",
                    Wait::Text => "after text appears",
                };
                egui::ComboBox::from_id_salt(("profile-wait", i)).width(150.0).selected_text(when).show_ui(ui, |ui| {
                    ui.selectable_value(&mut step.wait, Wait::Nothing, if i == 0 { "when the shell is ready" } else { "right away" });
                    ui.selectable_value(&mut step.wait, Wait::Prompt, "after a prompt");
                    ui.selectable_value(&mut step.wait, Wait::Text, "after text appears");
                });
                match step.wait {
                    Wait::Prompt => {
                        ui.add(egui::TextEdit::singleline(&mut step.value).desired_width(70.0).hint_text(PROMPT_ENDS))
                            .on_hover_text("The line with the cursor must end with one of these (| separates them)");
                    }
                    Wait::Text => {
                        ui.add(egui::TextEdit::singleline(&mut step.value).desired_width(70.0).hint_text("text"));
                    }
                    Wait::Nothing => {}
                }
                ui.checkbox(&mut step.enter, "⏎").on_hover_text("Press Enter after typing it");
                if ui.add_enabled(i > 0, egui::Button::new("↑").small()).clicked() {
                    swap = Some(i - 1);
                }
                if ui.add_enabled(i + 1 < count, egui::Button::new("↓").small()).clicked() {
                    swap = Some(i);
                }
                if ui.add(egui::Button::new("✕").small()).clicked() {
                    remove = Some(i);
                }
            });
        }
    });
    if let Some(i) = swap {
        d.steps.swap(i, i + 1);
    }
    if let Some(i) = remove {
        d.steps.remove(i);
    }
    if ui.button("+ Add command").clicked() {
        let mut step = StepDraft::new(String::new());
        if !d.steps.is_empty() {
            step.wait = Wait::Prompt;
        }
        d.steps.push(step);
    }
    ui.add_space(6.0);
    ui.label(
        RichText::new("{prompt:Task} asks for a value when it runs (add |q to quote it). Also {uuid} {short} {words} {date} {time} {cwd} {cwd_name} {home} {env:NAME}.")
            .size(11.5)
            .color(pal.dim),
    );
}

/// The text of a terminal row, with the gaps between runs as spaces.
fn line_text(line: &Line, upto: Option<u16>) -> String {
    let mut out = String::new();
    let mut col = 0u16;
    for run in &line.runs {
        if upto.is_some_and(|end| run.col >= end) {
            break;
        }
        while col < run.col {
            out.push(' ');
            col += 1;
        }
        match upto {
            Some(end) if run.col + run.width > end => out.extend(run.text.chars().take((end - run.col) as usize)),
            _ => out.push_str(&run.text),
        }
        col = run.col + run.width;
    }
    out
}

/// The cursor's line up to the cursor: the prompt, before anything is typed.
fn before_cursor(frame: &Frame) -> String {
    let Some(c) = frame.cursor else { return String::new() };
    frame.lines.get(c.row as usize).map(|l| line_text(l, Some(c.col))).unwrap_or_default()
}

/// The whole logical line the cursor is on, joining the rows a long command wrapped onto.
fn cursor_line(frame: &Frame) -> String {
    let Some(c) = frame.cursor else { return String::new() };
    let row = c.row as usize;
    if row >= frame.lines.len() {
        return String::new();
    }
    let mut first = row;
    while first > 0 && frame.lines[first - 1].wrapped {
        first -= 1;
    }
    frame.lines[first..=row].iter().map(|l| line_text(l, None)).collect()
}

/// The command on a prompt line.
fn strip_prompt(line: &str, prompt: &str) -> String {
    let prompt = prompt.trim_end();
    let rest = match line.strip_prefix(prompt) {
        Some(rest) if !prompt.is_empty() => rest,
        // The screen moved under the prompt: cut at the first usual prompt ending.
        _ => ["$ ", "% ", "# ", "> ", "❯ "].iter().filter_map(|e| line.find(e).map(|i| i + e.len())).min().map_or(line, |i| &line[i..]),
    };
    // A TUI input box (Claude Code's) has a border on the right.
    rest.trim().trim_end_matches('│').trim().to_string()
}

/// The prompt ending to wait for before typing the command that followed `prompt`.
fn prompt_end(prompt: &str) -> String {
    match prompt.trim_end().chars().last() {
        Some(c) if "$%#>❯".contains(c) => c.to_string(),
        _ => PROMPT_ENDS.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_round_trip() {
        let steps = [
            Step::Run("ssh -t dev".into()),
            Step::Detailed { run: "cd x".into(), wait_for: Some("$".into()), wait_text: None, delay_ms: 0, timeout_ms: None, enter: None },
            Step::Detailed { run: "y".into(), wait_for: None, wait_text: Some("Continue?".into()), delay_ms: 200, timeout_ms: Some(5000), enter: Some(false) },
        ];
        for step in steps {
            assert_eq!(StepDraft::from_step(&step).to_step(), step);
        }
    }

    #[test]
    fn commands_come_off_their_prompt() {
        assert_eq!(strip_prompt("(base) me@mac ~ % ssh -t dev", "(base) me@mac ~ % "), "ssh -t dev");
        assert_eq!(strip_prompt("me@dev:~$ cd /srv && ls", "something else $ "), "cd /srv && ls");
        assert_eq!(strip_prompt("│ > fix the bug        │", "│ > "), "fix the bug");
        assert_eq!(prompt_end("me@dev:~$ "), "$");
        assert_eq!(prompt_end("→ "), PROMPT_ENDS);
    }
}

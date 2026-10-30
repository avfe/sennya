---
name: sennya
description: Drive the sennya terminal from inside it. Open a side task in a new Claude Code session next to you, run sennya profiles, read or type into other terminals, arrange panes, resume past Claude Code chats, and edit sennya's config (profiles, shortcuts, themes). Use when $SENNYA_SESSION is set and the user wants work done in another session, terminal, pane, or tab; when they mention sennya; or when they ask to set up a sennya shortcut, profile, or setting.
---

# sennya

sennya is the user's terminal. Its sessions live in a background daemon, and the `sennya`
command controls the app: it opens terminals, types into them, reads their output, and
changes the layout. Commands print JSON (`read` prints plain text).

You are inside sennya when `$SENNYA_SESSION` is set. In any command, `current` means your own
terminal. If `sennya` is missing from `PATH`, try
`/Applications/Sennya.app/Contents/MacOS/sennya`.

## Start a task in a separate Claude Code session

When the user wants something done "in a separate session" or "on the side", open Claude
Code next to yourself with the task already submitted:

```sh
sennya claude --near current --open stack --skip-permissions "<self-contained task prompt>"
```

- The new session knows nothing about this conversation. Write the prompt as a complete brief:
  the goal, the relevant files and paths, constraints, what "done" means, and anything the
  user already decided.
- `--open stack` keeps your terminal as the big pane on the left and stacks tasks on the
  right. Use `--open right` or `--open down` to split beside you, `--open page` for a new page,
  and `--open tab` for a tab in the current page.
- Only add `--skip-permissions` (bypass permissions) when the user has asked for it or
  usually works that way. Otherwise leave it out.
- `--no-submit` types the prompt without pressing Enter, so the user can review it first.
  Use it when the user wants to check or edit the prompt.
- `--cwd DIR` sets the working directory (default: yours), `--title NAME` sets the pane title,
  and `--model MODEL` picks the model.
- The command prints `{"id": N}`. Tell the user the task was opened and where it is.

To check on it later, run `sennya read N --lines 80`. Don't type into another Claude
session unless the user asks.

## Control terminals

```sh
sennya list                                   # pages, terminals, titles, folders, programs, layout
sennya new --near current --open right -- npm run dev    # run a command in a new pane
sennya send ID "make test"                    # type a line and press Enter (--no-enter to just type)
sennya read ID --lines 100                    # recent output
sennya focus ID | hide ID | show ID | close ID
sennya layout main-stack                      # grid | columns | rows | main-stack | manual
```

- `hide` takes a terminal out of the grid while it keeps running. Use it for noisy
  background jobs the user doesn't need to watch.
- Ask before you `close` a terminal you didn't open, or one where a program is running
  (the `process` field in `sennya list`).
- When you start a long-running command for the user, prefer a new pane (`sennya new`) so
  your own terminal stays free.

## Profiles

Profiles are recipes in the config that open a terminal and run steps, for example ssh to a
server, create a fresh task folder, and start Claude.

```sh
sennya profiles                               # names, shortcuts, and the {prompt:...} values each needs
sennya run dev-task --near current --var Task="Fix the flaky login test"
```

If a profile has a `{prompt:X}` placeholder, pass `--var X=value`. When the user's workflow
matches an existing profile (same server, same folder scheme), prefer running that profile
over typing the commands yourself.

## Claude Code history

```sh
sennya history --limit 20                     # chats with id, title, cwd, branch, last prompt
sennya resume <id-or-prefix>                  # reopen a chat in a new terminal
```

## Servers

A server can run its own sennya daemon. Its terminals then live on the server and survive
network drops, laptop sleep and restarts of the window.

```sh
sennya remote install HOST     # once: put the daemon on HOST (anything you can `ssh HOST` to)
sennya remote HOST             # open a window attached to it; it reconnects by itself
```

- A Mac server with the same kind of chip gets a copy of this binary. Any other server builds
  the daemon from source with its own Rust; `--install-rust` installs Rust there first
  (rustup, no sudo).
- The connection runs `sennya proxy` over plain `ssh HOST` (from `~/.sennya/bin`, else from
  PATH), so keys, `~/.ssh/config` and jump hosts work as usual. It must work without a password prompt (keys or ssh-agent).
- On the server, `sennya` is in `~/.local/bin` and Claude Code there gets this skill too.

## Configure sennya

The config is `~/.config/sennya/config.toml` (`sennya config path`). sennya reloads it as
soon as it is saved, and `sennya config example` prints the fully commented default.

When you edit the config, change only what the user asked for and keep everything else as
it is. Validate your change with `sennya profiles`: it fails with the parse error if the
TOML is broken.

### Profile format

```toml
[[profile]]
name = "dev-task"
description = "ssh dev, fresh task folder, Claude with the task"
key = "cmd+shift+t"          # a profile key overrides any built-in shortcut on that chord
open = "right"               # tab | right | down | stack | page
page = "dev {words}"         # page name when open = "page"
title = "task {short}"
cwd = "~"
steps = [
  "ssh -t dev",
  { wait_for = "$|%|#", run = "mkdir -p ~/agents/workdirs/{cwd_name}/task_{uuid} && cd ~/agents/workdirs/{cwd_name}/task_{uuid}" },
  { wait_for = "$|%|#", run = "claude --dangerously-skip-permissions {prompt:Task|q}" },
]
```

- A step is either a string (typed, then Enter) or a table with these keys:
  - `run`: the text to type.
  - `wait_for`: the cursor line must end with one of the `|`-separated texts, such as a
    shell prompt.
  - `wait_text`: text that must appear on the screen.
  - `delay_ms`, `timeout_ms`.
  - `enter = false`: type without pressing Enter.
- Placeholders get one value per run: `{uuid}` `{short}` (8 hex chars) `{words}`
  (brave-otter-42) `{timestamp}` (20261029-231502) `{date}` `{time}` `{unix}` `{home}` `{cwd}`
  `{cwd_name}` `{env:NAME}` `{prompt:Label}`. A `{prompt:Label}` placeholder is asked for in
  the UI or passed with `--var`. Append `|q` to shell-quote a value.
- Shortcut chords look like `cmd+shift+t`, `ctrl+alt+k`, or `f5`. `cmd` means Ctrl on Linux.

### Other settings

- `[theme]`: set `name` to one of `sennya`, `true-black`, `tokyo-night`, `catppuccin-mocha`,
  `dracula`, `nord`, `gruvbox-dark`, `one-dark`, `solarized-dark`, `catppuccin-latte`, or
  `github-light`. Override single colors with `[theme.colors]`, for example
  `background = "#000000"` or `accent = "#2aabee"`.
- `[font]`: `family` (a comma-separated fallback list), `size`, `line_height`.
- `[keys]`: lines like `action = "chord"`, for example `new_terminal = "cmd+t"` or
  `hide_pane = "cmd+shift+m"`. Use `"none"` to unbind. The actions are listed in sennya's
  Settings → Keyboard shortcuts.
- `[shell]` (`program`, `args`), `[env]`, `[terminal]` (`scrollback`, `cursor_style`,
  `option_as_alt`, `shift_enter`), `[clipboard]` (`image_paste = "path" | "ctrl-v"`),
  `[persistence]` (`save_scrollback`, `resume_claude`), and `[ui]` (`default_mode`,
  `pane_gap`, `padding`).

## Good manners

- Explain what you opened or changed in one line, including the terminal id.
- Never run `sennya kill` or `sennya restart-daemon` without asking the user. They stop every
  program running in sennya.

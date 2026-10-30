# sennya for agents

sennya is a terminal with a scriptable CLI. If `$SENNYA_SESSION` is set, you are running
inside a sennya terminal, and `current` refers to that terminal in every command below.
Commands print JSON (`read` prints plain text). `sennya --help` lists them all.

## Start a side task in a new Claude Code session

Open Claude Code next to yourself, in the same folder, with the task already submitted:

```sh
sennya claude --near current --open stack --skip-permissions "Write tests for src/parser.rs"
```

- `--open stack`: this terminal stays the big pane on the left and new tasks stack on the
  right. Other placements: `right`, `down`, `tab` (current page), `page` (new page).
- `--no-submit`: type the prompt without pressing Enter, so the user can review it first.
- `--cwd DIR`, `--title NAME`, `--model MODEL`.
- Without `--skip-permissions` Claude Code asks for permissions as usual.

The command prints `{"id": N}`. Follow the task with `sennya read N --lines 80`.

## Control terminals

| Command | What it does |
|---|---|
| `sennya list` | pages, terminals, titles, folders, running programs |
| `sennya new [--near ID] [--open WHERE] [--cwd DIR] [--title T] [-- CMD...]` | open a terminal and run CMD |
| `sennya send ID TEXT... [--no-enter]` | type into a terminal |
| `sennya read ID [--lines N]` | last N lines of output |
| `sennya focus ID` / `hide ID` / `show ID` / `close ID` | show, hide from the grid (it keeps running), close |
| `sennya layout grid\|columns\|rows\|main-stack\|manual` | how the current page tiles |
| `sennya run PROFILE [--near ID] [--var Label=value]...` | run a profile from the config |
| `sennya profiles` | profiles and the `{prompt:...}` values they need |
| `sennya history [--limit N]` | Claude Code chats, newest first, with their resume ids |
| `sennya resume ID` | reopen a Claude Code chat in a new terminal |

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

The config is `~/.config/sennya/config.toml` (`sennya config path`). It is reloaded as soon
as it is saved; `sennya config example` prints a fully commented default. Edit it with care:
keep the user's existing settings and only add or change what was asked.

Profiles turn a shortcut into a recipe:

```toml
[[profile]]
name = "dev-task"
description = "New Claude task on the dev server"
key = "cmd+shift+t"          # any chord; profile keys override built-in shortcuts
open = "right"               # tab | right | down | stack | page
title = "task {short}"
steps = [
  "ssh -t dev",
  { wait_for = "$|%|#", run = "mkdir -p ~/agents/workdirs/{cwd_name}/task_{uuid} && cd $_" },
  { wait_for = "$|%|#", run = "claude --dangerously-skip-permissions {prompt:Task|q}" },
]
```

- Placeholders: `{uuid}` `{short}` `{words}` `{timestamp}` `{date}` `{time}` `{unix}`
  `{home}` `{cwd}` `{cwd_name}` `{env:NAME}` `{prompt:Label}` (asked when the profile runs;
  pass `--var Label=value` from the CLI). Add `|q` to shell-quote a value.
- A step is a string (typed, then Enter) or a table with `run`, `wait_for` (the cursor line
  must end with one of the `|`-separated texts, e.g. a shell prompt), `wait_text` (text that
  must appear on screen), `delay_ms`, `timeout_ms`, `enter = false`.
- Other sections: `[font]`, `[theme]` (`name` and `[theme.colors]` overrides), `[shell]`,
  `[env]`, `[terminal]`, `[clipboard]`, `[persistence]`, `[ui]`, `[security]`,
  `[keys]` (`action = "cmd+shift+x"`; actions are listed in Settings → Keyboard shortcuts).

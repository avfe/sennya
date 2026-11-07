# sennya

**English** | [Русский](README.ru.md)

A fast terminal written in Rust with GPU rendering (wgpu/Metal). Sessions live in a separate
background process, so closing the window, `kill -9` or a crash of the window never kills a shell.
Even if the daemon itself dies (or the Mac restarts), terminals come back with their history and
layout. It is built for working with AI agents: Claude Code sessions are tracked, resumed after
restarts, and Claude can drive the terminal through a CLI.

macOS is the main platform; Windows builds work too (see [Windows](#windows)).

## Install

**From a release.** Download the archive for your system from
[Releases](https://github.com/avfe/sennya/releases):

- **macOS**: `Sennya-macos-universal.zip`. Unzip it and double-click `install.command`. It copies
  `Sennya.app` to `/Applications`, puts the `sennya` command on your `PATH` and installs the Claude
  Code skill. The app is not notarized, so if you open `Sennya.app` directly the first time, use
  right-click → Open.
- **Windows**: `sennya-windows-x64.zip`. Unzip it and run
  `powershell -ExecutionPolicy Bypass -File install-windows.ps1`. It installs to
  `%LOCALAPPDATA%\Programs\sennya`, adds it to `PATH` and creates a Start menu shortcut.

**From source** (macOS, needs Rust):

```sh
./scripts/install-macos.sh
```

The script builds `Sennya.app`, puts it in `/Applications`, makes the `sennya` command available
on your `PATH` and installs the skill for Claude Code. To undo:
`rm -rf /Applications/Sennya.app /opt/homebrew/bin/sennya ~/.claude/skills/sennya`.

**The Claude Code skill comes with every install.** It lives in `~/.claude/skills/sennya` and
teaches Claude to open terminals, run tasks next to itself, write profiles and change settings.
The daemon refreshes it on every start whenever `~/.claude` exists, and `sennya install-skill`
installs it by hand.

```
sennya            open a window (the daemon starts by itself)
sennya --help     every CLI command
sennya kill       stop the daemon and every terminal
```

After an update the window offers to restart the daemon. Terminals come back in the same folders
with their history and Claude Code chats resume; other programs running in them stop.

## Features

- **Two modes** (`⌘G` switches):
  - **Grid**: every terminal of a page is visible at once. A new terminal fits itself into the
    grid, dividers move with the mouse, and panes are dragged by their header (to the edge of
    another pane, or onto its center to swap the two).
  - **Tabs**: vertical tabs on the left, one terminal filling the window on the right.
- **Pages**: separate grids of terminals (`⌘N`, `⌘1…9`, `⌥⌘[` / `⌥⌘]`). Drag a terminal onto a page
  tab or onto the page header in the sidebar to move it. When there are more pages than fit, the
  tabs scroll sideways.
- **Page layouts**: Grid / Columns / Rows / Main + stack / Manual. Grid, Columns and Rows re-tile
  automatically; moving a divider switches the page to Manual.
- **Areas**: right-click a pane → "New terminals open in this area". After that `⌘T` adds terminals
  to that pane's group.
- **Search**: `⌘F` searches the current terminal, `⌘⇧F` the history of every terminal on every page,
  `⌘P` jumps to a terminal by name or folder (and runs profiles).
- **Clipboard**: `⌘C` / `⌘V`; bracketed paste that can't be broken out of; a confirmation before
  pasting several lines into a bare shell.
- **Screenshots and images**: `⌘V` with an image on the clipboard (after `⌘⌃⇧4`, say) saves it as a
  PNG and pastes the path. Claude Code shows such a paste as `[Image #1]`. `⌃V` goes to the program
  as is, so Claude Code's own image paste works too.
- **Drag and drop files** into the window to paste their escaped paths.
- **Clickable links**: `⌘`+click. Plain URLs (including ones wrapped onto the next line) and OSC 8
  hyperlinks.
- **Shift+Enter** inserts a newline in Claude Code without sending the message.
- Mouse wheel, selection (double click selects a word, triple a line, `⌥`+drag a block), mouse
  reporting for vim/htop, focus events, macOS "natural text editing" (`⌥←`, `⌘←`, `⌘⌫`), and
  shortcuts that work on non-Latin keyboard layouts.
- Activity and bell indicators on tabs, renaming (`F2` or double click).
- **Multi-select**: press and hold a terminal in the list (or `⌘`+click) to get checkboxes and a
  "Close N / Move to / All / Done" bar. `Esc` leaves the selection.
- **Collapsible pages** in the sidebar (click the page header); collapsed pages are remembered.
- The sidebar hides with the `«` button (and comes back with `»`) or `⌘B`.
- Flat design: grid panes have no frames or headers, they are separated by a 1 px line and the
  active one is marked with a bar on top. A "drag / maximize / close" handle appears when you hover
  the top of a pane.

## Profiles

A profile turns one shortcut into a recipe: it opens a terminal (a tab, a split, the stack or a new
page) and types commands, waiting for a prompt or for text on the screen between them. For
example: `ssh dev`, a fresh `task_{uuid}` folder, then `claude` with the task you type in.

Three ways to make one, all in ⚙ → Profiles:

- **The editor** (New profile… / Edit profiles…): name, shortcut, where it opens, the folder and the
  commands. For each command you choose when it is typed: once the shell is ready, right after the
  previous one, after a prompt (`$`, `%`, `#`, `>`) or after some text appears. "Save and run" tries
  it at once.
- **Recording**: press "Record from a terminal", type your commands in the focused terminal as
  usual, then press "Stop and edit". Each command becomes a step that waits for the prompt it was
  typed at. Passwords are never recorded, because the terminal does not show them.
- **Describe it to Claude**: opens Claude Code next to you with the request started. Say in your own
  words what the profile should do, and Claude writes it into the config with the sennya skill.

Placeholders: `{uuid}` `{short}` `{words}` `{timestamp}` `{date}` `{time}` `{cwd}` `{cwd_name}`
`{home}` `{env:NAME}`. `{prompt:Question}` is asked when the profile runs, and `|q` quotes a value for
the shell. Run a profile with its shortcut, from `⌘P`, from ⚙ → Profiles or with `sennya run name`.
Profiles are stored as `[[profile]]` entries in the config, so you can edit them there too.

## Agents and Claude Code

- **A CLI for agents** (JSON output): `sennya list / new / claude / run / send / read / focus / hide /
  show / close / layout / history / resume`. Inside a sennya terminal, `current` means that terminal.
  For example, Claude opens a task next to itself:
  `sennya claude --near current --open stack --skip-permissions "the task"`.
  The full guide for agents is `sennya help agents` (that is `AGENTS.md`).
- **Claude Code chats** (`⇧⌘H`): every conversation with its title, folder, branch and time; a click
  opens `claude --resume <id>` in a new terminal. A terminal remembers which chat ran in it, and
  after a restart or a crash of the daemon the chat resumes by itself
  (`persistence.resume_claude`), in the permission mode it was in (bypass stays bypass).
- **Hidden terminals** (`⇧⌘M` or "—" on the pane handle) keep running. They are shown dimmed in the
  sidebar and in a strip under the grid with an activity indicator; a click brings them back.
- **Several windows** (`⇧⌘N`): each window shows its own page and mode, the layout is shared.
- **Focus mode** (`⌃⌘F`): fullscreen with no sidebar or bars, only terminals.
- The **true-black** theme (pure black for OLED) is in ⚙ → Theme.

## Servers

A server can run its own sennya daemon. Its terminals then live on the server and survive network
drops, laptop sleep and restarts of the window, with nothing but SSH: no UDP ports as with mosh.

```sh
sennya remote install dev   # once: put the daemon on the server (any host you can `ssh` to)
sennya remote dev           # open a window attached to it; it reconnects by itself
```

- The window pings the server every 2 seconds. When the network drops (a Wi-Fi switch, a VPN,
  sleep) it notices within about 6 seconds and keeps reconnecting until the server is back. Every
  terminal is where you left it: the shells never noticed.
- The link is plain `ssh dev sennya proxy`, so keys, `~/.ssh/config` and jump hosts work as usual.
  It must log in without a password prompt (a key or ssh-agent). Only changed screen lines travel
  over the network.
- A Mac server gets a copy of this binary; on Linux the daemon is built from source
  (`--install-rust` installs Rust with rustup, no sudo).
- ⚙ → Servers lists the servers from `[[remote]]` in the config and has "Connect to a server…".

## Windows

`sennya.exe` is the same program: the daemon (ConPTY sessions over an AF_UNIX socket), the window
and the CLI. The default shell is PowerShell; `Ctrl+C` copies when text is selected and interrupts
otherwise, `Ctrl+V` pastes. To cross-compile from a Mac: `./scripts/build-windows.sh` (needs
[llvm-mingw](https://github.com/mstorsjo/llvm-mingw)).

## Performance

- The daemon emulates the terminals (`alacritty_terminal`). The window gets frames **only for
  visible** terminals and at most ~140 times a second, so 50 open sessions cost next to nothing.
- Idle, both processes use 0% CPU: drawing happens only on events.
- In a test (debug build) `seq 1 300000` finished in under a second. Peak CPU: window 10%, daemon
  55%, then 0% right after.

## Security

- The socket and every file are private (`0700`/`0600`), and the daemon checks the UID of whoever
  connects.
- Programs can only write to the clipboard (OSC 52); reading it is not allowed.
- Bracketed paste can't be "closed" from inside the pasted text.
- Links open only for http/https/file/ftp/mailto.
- Saving scrollback to disk can be turned off (`persistence.save_scrollback = false`).
- If the daemon outlives your macOS login session (you logged out and back in), its terminals can no
  longer reach system services such as DNS, the keychain and certificate checks. The window notices
  and offers to restart it.

## Configuration

`~/.config/sennya/config.toml` is created on the first start with comments, and changes apply as
soon as you save. It sets the font (a chain of families), the theme (11 built in, any color can be
overridden), the shell, environment variables, the cursor, `option_as_alt`, how images are pasted,
saving history, profiles and any shortcut (`[keys]`). The theme can also be changed from the ⚙ menu.

## Shortcuts (macOS)

Change any of them in ⚙ → Keyboard shortcuts: click a shortcut and press the new one (Esc cancels,
⌫ removes it, Reset restores the default). Changes are written to `[keys]` in the config.

| Action | Keys |
|---|---|
| New terminal | `⌘T` |
| Close terminal | `⌘W` |
| Split right / down | `⌘D` / `⌘⇧D` |
| Tabs ↔ Grid | `⌘G` |
| New page / close page | `⌘N` / `⌘⇧W` |
| Page 1…9, next/previous | `⌘1…9`, `⌥⌘]` / `⌥⌘[` |
| Next/previous terminal | `⇧⌘]` / `⇧⌘[` |
| Focus the neighboring pane | `⌘⌥←↑→↓` |
| Maximize pane | `⌘⇧↩` |
| Rebalance the grid | `⌘⇧R` |
| Sidebar | `⌘B` |
| Find / in all / jump | `⌘F` / `⌘⇧F` / `⌘P` |
| Clear history | `⌘K` |
| Font size | `⌘=` / `⌘-` / `⌘0` |
| Rename | `F2` |
| Config | `⌘,` |
| Hide pane (keeps running) | `⇧⌘M` |
| Claude Code chats | `⇧⌘H` |
| New window | `⇧⌘N` |
| Focus mode (fullscreen) | `⌃⌘F` |
| Quit (terminals keep running) | `⌘Q` |

## Architecture

```
 window (winit + wgpu + egui)  ── Unix socket, postcard ──  daemon
   layout, input, rendering                                   a PTY + alacritty_terminal per session
   visible panes only                                         frames of visible sessions, search,
                                                              autosave of state.json + scrollback
```

- `src/daemon`: sessions, sending frames, saving and restoring.
- `src/gui`: the window, UI, keyboard and mouse, drawing the grid.
- `src/model.rs`: pages and the split tree (shared by the window and the daemon).
- `src/protocol.rs`: messages between the processes.
- `src/sys.rs`: what differs between macOS/Linux and Windows.

Tests: `cargo test` (unit tests and end-to-end tests with a real daemon, shell, `kill -9` and
restore).

## Next

- Showing IME pre-edit text for Chinese and Japanese input.
- `⌘`+click on paths like `src/main.rs:42` to open them in an editor.

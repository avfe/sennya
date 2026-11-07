## Downloads

| System | File |
|---|---|
| macOS 11 or later, Apple Silicon and Intel | `Sennya-macos-universal.zip` |
| Windows 10 (1803 or later) or 11, 64-bit | `sennya-windows-x64.zip` |

Both include the `sennya` command and the **Claude Code skill**: the installers run
`sennya install-skill`, which writes `~/.claude/skills/sennya/SKILL.md`. With it, a Claude Code
agent can open terminals and side tasks, run profiles and edit your sennya config. The skill is
built into the binary, and the sennya daemon refreshes it every time it starts if `~/.claude`
exists, so it stays in step with the version you run. Run `sennya install-skill` to write it
again at any time.

## Install on macOS

1. Unzip `Sennya-macos-universal.zip` (Safari does this for you). You get a `Sennya` folder with
   `Sennya.app` and `install.command`.
2. Double-click `install.command`. It:
   - copies Sennya.app to /Applications;
   - removes the quarantine flag from it (`xattr -dr com.apple.quarantine`), so the app opens without
     Gatekeeper warnings;
   - links the `sennya` command into the first writable one of /opt/homebrew/bin, /usr/local/bin,
     ~/.local/bin;
   - installs the Claude Code skill.
3. Open Sennya from Applications or Spotlight.

**If macOS says it "cannot be opened" or "could not verify" the file.** Sennya is ad-hoc signed, not
notarized by Apple, so Gatekeeper stops the first double-click of anything downloaded. Any one of
these gets past it:

- In Terminal, which works on every macOS version:
  `bash ~/Downloads/Sennya/install.command` (adjust the path if you unzipped somewhere else).
- macOS 14 and earlier: right-click `install.command`, choose **Open**, then **Open** again.
- macOS 15 and later: double-click it, close the warning, then go to **System Settings → Privacy &
  Security**, click **Open Anyway** next to "install.command", and confirm.

Without the script: drag Sennya.app to /Applications, open it with right-click → Open (or Open
Anyway as above), and run `/Applications/Sennya.app/Contents/MacOS/sennya install-skill` for the
skill.

## Install on Windows

1. Right-click `sennya-windows-x64.zip` → **Extract All**.
2. Open PowerShell in the extracted folder (type `powershell` in the Explorer address bar and press
   Enter) and run:

   ```powershell
   powershell -ExecutionPolicy Bypass -File install-windows.ps1
   ```

   It installs for your user only, without admin rights:
   - copies `sennya.exe` to `%LOCALAPPDATA%\Programs\sennya` and unblocks it, so SmartScreen does not
     stop it;
   - adds that folder to your user PATH (open a new terminal to pick it up);
   - creates a **Sennya** shortcut in the Start menu;
   - installs the Claude Code skill.
3. Start Sennya from the Start menu.

The exe has the C runtime built in, so it needs no Visual C++ redistributable. It is not code-signed:
if you start `sennya.exe` from the zip directly and SmartScreen shows "Windows protected your PC",
click **More info → Run anyway**.

## Update

Download the new zip and install it the same way, over the old version. Your terminals live in the
sennya daemon, which keeps running the old version. To move them to the new one, run:

```sh
sennya restart-daemon
```

They come back in the same folders with their history; programs running in them stop, so let them
finish first. If the new app finds a daemon too old to talk to, it offers this restart itself. The
installers and the new daemon both refresh the Claude Code skill.

## Uninstall

- macOS: `sennya kill`, then `rm -rf /Applications/Sennya.app "$(command -v sennya)" ~/.claude/skills/sennya`
- Windows: `sennya kill`, then delete `%LOCALAPPDATA%\Programs\sennya`, the Sennya shortcut in the
  Start menu and `%USERPROFILE%\.claude\skills\sennya`, and remove the folder from your user PATH.

## Checksums

`SHA256SUMS.txt` lists the SHA-256 of both zips. Check a download with
`shasum -a 256 -c SHA256SUMS.txt --ignore-missing` on macOS, or compare
`Get-FileHash sennya-windows-x64.zip` with the list on Windows.

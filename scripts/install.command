#!/bin/bash
# Install Sennya from the release zip, where this script sits next to Sennya.app.
# Double-click it in Finder, or run `bash install.command` in Terminal. It:
#   - copies Sennya.app to /Applications (asks for your password only if that folder is not writable);
#   - removes the quarantine flag, so the ad-hoc signed app opens without Gatekeeper prompts;
#   - links the `sennya` CLI into /opt/homebrew/bin, /usr/local/bin or ~/.local/bin, the first writable one;
#   - installs the Claude Code skill (`sennya install-skill`), so Claude Code agents can drive sennya.
# Undo: rm -rf /Applications/Sennya.app "$(command -v sennya)" ~/.claude/skills/sennya
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
SRC="$HERE/Sennya.app"
DEST=/Applications/Sennya.app

if [ ! -d "$SRC" ]; then
  echo "Sennya.app is not next to this script (in $HERE)." >&2
  echo "Unzip the whole Sennya-macos-universal.zip and run install.command from the Sennya folder." >&2
  exit 1
fi

SUDO=
if [ ! -w /Applications ] || { [ -e "$DEST" ] && [ ! -w "$DEST" ]; }; then
  echo "/Applications is not writable for $(id -un); using sudo"
  SUDO=sudo
fi
if [ "$SRC" -ef "$DEST" ]; then
  echo "Sennya.app is already in /Applications"
else
  $SUDO rm -rf "$DEST"
  $SUDO ditto "$SRC" "$DEST"
fi
# Downloaded files carry com.apple.quarantine; without it Gatekeeper does not stop the app.
$SUDO xattr -dr com.apple.quarantine "$DEST" 2>/dev/null || true
echo "Installed $DEST"

# Same places as scripts/install-macos.sh.
BIN=/opt/homebrew/bin
[ -w "$BIN" ] || BIN=/usr/local/bin
[ -w "$BIN" ] || { BIN="$HOME/.local/bin"; mkdir -p "$BIN"; }
ln -sf "$DEST/Contents/MacOS/sennya" "$BIN/sennya"
echo "Linked $BIN/sennya"
case ":$PATH:" in
  *":$BIN:"*) ;;
  *) echo "  $BIN is not on your PATH; add it with: echo 'export PATH=\"$BIN:\$PATH\"' >> ~/.zshrc" ;;
esac

SKILL=$("$BIN/sennya" install-skill | sed -n 's/.*"path": *"\([^"]*\)".*/\1/p')
echo "Installed the Claude Code skill${SKILL:+ at $SKILL}"

if pgrep -f 'sennya daemon' >/dev/null 2>&1; then
  echo
  echo "The sennya daemon started before this install is still running. To move its terminals"
  echo "to the version you just installed, run:"
  echo "  sennya restart-daemon"
  echo "Terminals come back in their folders with their history; programs running in them stop."
fi
echo
echo "Done. Open Sennya from Applications or Spotlight."

#!/usr/bin/env bash
# Build Sennya.app, install it to /Applications, put the `sennya` CLI on PATH and
# install the Claude Code skill. Undo: rm -rf /Applications/Sennya.app "$BIN/sennya" ~/.claude/skills/sennya
set -euo pipefail
cd "$(dirname "$0")/.."
./scripts/bundle-macos.sh
rm -rf /Applications/Sennya.app
cp -R target/release/bundle.noindex/Sennya.app /Applications/
BIN=/opt/homebrew/bin
[ -w "$BIN" ] || BIN=/usr/local/bin
[ -w "$BIN" ] || { BIN="$HOME/.local/bin"; mkdir -p "$BIN"; }
ln -sf /Applications/Sennya.app/Contents/MacOS/sennya "$BIN/sennya"
"$BIN/sennya" install-skill >/dev/null
echo "Installed /Applications/Sennya.app, $BIN/sennya and the Claude Code skill"

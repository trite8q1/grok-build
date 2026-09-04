#!/usr/bin/env bash
# Install a daily LaunchAgent that rebases this checkout onto official grok-build.
# Generates the plist from this machine's HOME and repo path. Do not copy a
# checked-in plist from another computer.
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "error: LaunchAgent is macOS only" >&2
  exit 1
fi

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LABEL="ai.x.grok-local-update"
PLIST_DST="$HOME/Library/LaunchAgents/${LABEL}.plist"
STATE_DIR="${GROK_LOCAL_STATE:-$HOME/.local/state/grok-local}"
CARGO_BIN="${CARGO_HOME:-$HOME/.cargo}/bin"
UPDATE_SH="$ROOT/scripts/update-from-upstream.sh"

xml_escape() {
  printf '%s' "$1" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g'
}

mkdir -p "$HOME/Library/LaunchAgents" "$STATE_DIR"
chmod +x "$ROOT/scripts/update-from-upstream.sh" "$ROOT/scripts/install-grok-local.sh"

ROOT_X="$(xml_escape "$ROOT")"
HOME_X="$(xml_escape "$HOME")"
STATE_X="$(xml_escape "$STATE_DIR")"
CARGO_X="$(xml_escape "$CARGO_BIN")"
UPDATE_X="$(xml_escape "$UPDATE_SH")"
PATH_X="$(xml_escape "$CARGO_BIN:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin")"

cat >"$PLIST_DST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>${LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>/bin/bash</string>
    <string>${UPDATE_X}</string>
  </array>
  <key>WorkingDirectory</key>
  <string>${ROOT_X}</string>
  <key>StartInterval</key>
  <integer>86400</integer>
  <key>RunAtLoad</key>
  <false/>
  <key>StandardOutPath</key>
  <string>${STATE_X}/launchd.out.log</string>
  <key>StandardErrorPath</key>
  <string>${STATE_X}/launchd.err.log</string>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key>
    <string>${PATH_X}</string>
    <key>HOME</key>
    <string>${HOME_X}</string>
  </dict>
</dict>
</plist>
EOF

uid="$(id -u)"
launchctl bootout "gui/${uid}/${LABEL}" 2>/dev/null || true
launchctl bootstrap "gui/${uid}" "$PLIST_DST"
echo "Installed LaunchAgent ${LABEL} (daily) for $ROOT"
echo "Plist: $PLIST_DST"
echo "Run once now: $UPDATE_SH"
echo "Unload later: launchctl bootout gui/${uid}/${LABEL}"

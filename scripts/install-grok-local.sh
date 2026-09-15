#!/usr/bin/env bash
# Build this fork and install `g` on PATH, without touching official
# `~/.grok/bin/grok`. `grok-local` remains a symlink to `g`.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN_DIR="${GROK_LOCAL_BIN:-$HOME/.local/bin}"
LIB_DIR="${GROK_LOCAL_LIB:-$HOME/.local/lib/grok-local}"
CMD_NAME="${GROK_LOCAL_NAME:-g}"

cd "$ROOT"
if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo not on PATH" >&2
  exit 1
fi
if ! command -v dotslash >/dev/null 2>&1; then
  echo "error: dotslash not on PATH (needed for protoc). cargo install dotslash" >&2
  exit 1
fi
echo "Building xai-grok-pager-bin (release) in $ROOT"
cargo build -p xai-grok-pager-bin --release

mkdir -p "$BIN_DIR" "$LIB_DIR"
cp "$ROOT/target/release/xai-grok-pager" "$LIB_DIR/xai-grok-pager"
chmod +x "$LIB_DIR/xai-grok-pager"

cat > "$BIN_DIR/$CMD_NAME" <<EOF
#!/bin/sh
# Forked Grok Build with file-restore rewind. Does not replace official grok.
export GROK_DISABLE_AUTOUPDATER=1

if [ -z "\${GROK_THEME:-}" ] && [ -z "\${LC_GROK_THEME:-}" ] \
  && [ -L "\$HOME/.config/ghostty/surface/current" ] \
  && [ "\$(basename "\$(readlink "\$HOME/.config/ghostty/surface/current")")" = glass ]; then
  export GROK_TERMINAL_THEME="\${GROK_TERMINAL_THEME:-1}"
  export GROK_THEME=terminal
fi

exec "$LIB_DIR/xai-grok-pager" "\$@"
EOF
chmod +x "$BIN_DIR/$CMD_NAME"

if [[ "$CMD_NAME" != "grok-local" ]]; then
  ln -sfn "$CMD_NAME" "$BIN_DIR/grok-local"
fi

echo "Installed $BIN_DIR/$CMD_NAME"
echo "Run: $CMD_NAME"
echo "Official grok is unchanged at ~/.grok/bin/grok"
if ! echo ":$PATH:" | grep -q ":$BIN_DIR:"; then
  echo "Note: $BIN_DIR is not on PATH. Add it, or run $BIN_DIR/$CMD_NAME"
fi

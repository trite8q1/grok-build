#!/usr/bin/env bash
# New-machine bootstrap for `g`.
#   git clone https://github.com/trite8q1/grok-build.git
#   cd grok-build
#   ./scripts/setup.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
BIN_DIR="${GROK_LOCAL_BIN:-$HOME/.local/bin}"

ensure_upstream() {
  if git remote get-url upstream >/dev/null 2>&1; then
    git remote set-url upstream https://github.com/xai-org/grok-build.git
  else
    git remote add upstream https://github.com/xai-org/grok-build.git
  fi
  git remote set-url --push upstream no_push
}

ensure_branch() {
  local current
  current="$(git branch --show-current || true)"
  if [[ "$current" == "g" ]]; then
    return 0
  fi
  if git show-ref --verify --quiet refs/heads/g; then
    git checkout g
    return 0
  fi
  if git show-ref --verify --quiet refs/remotes/origin/g; then
    git checkout -B g origin/g
    return 0
  fi
  echo "error: branch g not found. Clone https://github.com/trite8q1/grok-build.git (default branch is g)." >&2
  exit 1
}

ensure_tools() {
  if ! command -v cargo >/dev/null 2>&1 || ! command -v rustc >/dev/null 2>&1; then
    echo "error: rustup/cargo not on PATH." >&2
    echo "  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh" >&2
    echo "  then open a new shell and re-run $0" >&2
    exit 1
  fi
  if ! command -v dotslash >/dev/null 2>&1; then
    echo "Installing dotslash (needed for protoc)..."
    cargo install dotslash --locked
  fi
}

ensure_path() {
  case ":$PATH:" in
    *":$BIN_DIR:"*) return 0 ;;
  esac
  echo "Note: $BIN_DIR is not on PATH. Add this to ~/.zshrc (or ~/.zprofile):"
  echo "  export PATH=\"$BIN_DIR:\$PATH\""
  local rc="$HOME/.zshrc"
  if [[ -n "${ZDOTDIR:-}" ]]; then
    rc="$ZDOTDIR/.zshrc"
  fi
  if [[ "${SETUP_ADD_PATH:-1}" == "1" ]] && [[ -f "$rc" || ! -e "$rc" ]]; then
    if ! grep -qF "$BIN_DIR" "$rc" 2>/dev/null; then
      {
        echo ""
        echo "# grok fork (\`g\`)"
        echo "export PATH=\"$BIN_DIR:\$PATH\""
      } >>"$rc"
      echo "Appended PATH line to $rc (new shells only)."
    fi
  fi
}

if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  echo "error: run this from a clone of trite8q1/grok-build" >&2
  exit 1
fi

ensure_upstream
ensure_branch
ensure_tools
"$ROOT/scripts/install-grok-local.sh"
ensure_path

if [[ "$(uname -s)" == "Darwin" ]]; then
  "$ROOT/scripts/install-update-agent.sh"
else
  echo "Skipping LaunchAgent (macOS only). Rebuild later with scripts/install-grok-local.sh"
fi

echo
echo "Done. In a new terminal:"
echo "  g"
echo "Official grok is unchanged. Docs: $ROOT/FORK.md"

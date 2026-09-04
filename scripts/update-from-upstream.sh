#!/usr/bin/env bash
# Fetch official grok-build dumps (upstream), rebase branch `g`, rebuild `g`,
# then force-with-lease push the patch branch to origin (the GitHub fork).
#
# Remotes:
#   upstream  https://github.com/xai-org/grok-build.git   (fetch only)
#   origin    git@gh-trite8q1:trite8q1/grok-build.git     (your fork)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

UPSTREAM_REMOTE="${GROK_UPSTREAM_REMOTE:-upstream}"
UPSTREAM_REF="${GROK_UPSTREAM_REF:-main}"
ORIGIN_REMOTE="${GROK_ORIGIN_REMOTE:-origin}"
PATCH_BRANCH="${GROK_PATCH_BRANCH:-g}"
PUSH="${GROK_PUSH_ORIGIN:-1}"
STATE_DIR="${GROK_LOCAL_STATE:-$HOME/.local/state/grok-local}"
mkdir -p "$STATE_DIR"
LOG="$STATE_DIR/update.log"

log() {
  printf '%s %s\n' "$(date -u +'%Y-%m-%dT%H:%M:%SZ')" "$*" | tee -a "$LOG"
}

notify() {
  local title="$1" body="$2"
  log "$title: $body"
  if command -v osascript >/dev/null 2>&1; then
    local safe="${body//\"/\'}"
    osascript -e "display notification \"$safe\" with title \"$title\"" >/dev/null 2>&1 || true
  fi
}

push_patch_branch() {
  if [[ "$PUSH" != "1" ]]; then
    log "Skipping push (GROK_PUSH_ORIGIN=$PUSH)"
    return 0
  fi
  if ! git remote get-url "$ORIGIN_REMOTE" >/dev/null 2>&1; then
    log "No $ORIGIN_REMOTE remote; skip push"
    return 0
  fi
  git fetch "$ORIGIN_REMOTE" "$PATCH_BRANCH" || true
  log "Pushing $PATCH_BRANCH to $ORIGIN_REMOTE (force-with-lease)"
  git push --force-with-lease "$ORIGIN_REMOTE" "$PATCH_BRANCH"
}

if [[ -d "$ROOT/.git" ]] && git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  :
else
  notify "g update" "Not a git repo: $ROOT"
  exit 1
fi

if ! git remote get-url "$UPSTREAM_REMOTE" >/dev/null 2>&1; then
  notify "g update" "Missing remote $UPSTREAM_REMOTE. Expected xai-org/grok-build."
  exit 1
fi

if [[ -n "$(git status --porcelain)" ]]; then
  notify "g update" "Working tree dirty. Commit or stash first."
  exit 1
fi

current="$(git branch --show-current)"
if [[ "$current" != "$PATCH_BRANCH" ]]; then
  git checkout "$PATCH_BRANCH"
fi

log "Fetching $UPSTREAM_REMOTE $UPSTREAM_REF"
git fetch "$UPSTREAM_REMOTE" "$UPSTREAM_REF"

upstream="$(git rev-parse "$UPSTREAM_REMOTE/$UPSTREAM_REF")"
base="$(git merge-base HEAD "$upstream")"

if [[ "$base" == "$upstream" ]]; then
  log "Already up to date with $UPSTREAM_REMOTE/$UPSTREAM_REF"
  if ! push_patch_branch; then
    notify "g update" "Local patch is current, but push to $ORIGIN_REMOTE failed."
    exit 4
  fi
  exit 0
fi

log "Rebasing $PATCH_BRANCH onto $UPSTREAM_REMOTE/$UPSTREAM_REF ($upstream)"
if ! git rebase "$UPSTREAM_REMOTE/$UPSTREAM_REF"; then
  notify "g update" "Rebase conflict. Fix files, then: git add -u && git rebase --continue && $ROOT/scripts/install-grok-local.sh && git push --force-with-lease $ORIGIN_REMOTE $PATCH_BRANCH"
  exit 2
fi

log "Rebase clean. Building g."
if ! "$ROOT/scripts/install-grok-local.sh"; then
  notify "g update" "Build failed after rebase. See $LOG"
  exit 3
fi

if ! push_patch_branch; then
  notify "g update" "Rebuilt g, but push to $ORIGIN_REMOTE failed. See $LOG"
  exit 4
fi

notify "g update" "Rebased onto official grok-build, rebuilt g, pushed $PATCH_BRANCH."
exit 0

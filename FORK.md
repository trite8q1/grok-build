# Local fork (`g`)

GitHub fork: [trite8q1/grok-build](https://github.com/trite8q1/grok-build), from [xai-org/grok-build](https://github.com/xai-org/grok-build).

The only product change is that `/rewind` can restore files again. Same account, models, and `~/.grok` sessions as official Grok Build.

Official `grok` is not replaced. Open this build with `g`.

## New machine

Needs: Git, [rustup](https://rustup.rs), and a GitHub login that can clone (and, for the daily push, write to) this fork. Official `grok` is optional.

```bash
git clone https://github.com/trite8q1/grok-build.git
cd grok-build
./scripts/setup.sh
# new terminal
g
```

That checkout uses the default branch `g`. `setup.sh` adds `upstream`, installs the `g` command at `~/.local/bin/g`, and on macOS installs a daily LaunchAgent generated for **this** home directory and repo path.

For daily `git push` after rebase, origin must be writable (`gh auth login` + `gh auth setup-git`, or SSH). Fetch of official dumps is public HTTPS and needs no GitHub auth.

This Mac uses the `gh-trite8q1` SSH host alias. Other machines should not copy that; clone `https://github.com/trite8q1/grok-build.git` or `git@github.com:trite8q1/grok-build.git`.

Do not merge branch `g` into `main`. `main` stays a mirror of official `upstream/main`. The patch is one commit on branch `g`. The daily job rebases that commit onto `upstream/main` and force-with-lease pushes branch `g` to `origin`.

## Commands

| Command | What it is |
|---|---|
| `grok` | Official install at `~/.grok/bin/grok`. Update with `grok update`. |
| `g` | This fork at `~/.local/bin/g`. Auto-update of the official binary is disabled. |
| `grok-local` | Symlink to `g`. |

Both talk to `auth.x.ai` and reuse `~/.grok/auth.json`.

## Rewind

`/rewind`, `/undo`, or Esc Esc while idle with an empty prompt.

1. Pick a turn.
2. Choose what to restore:
   - `a` Both conversation and file changes
   - `c` Conversation only
   - `f` File changes only (hidden when that turn has no tracked edits)
3. If **Confirm before rewind** is on in `/settings`, confirm with Yes / Yes, and don't ask again / No. Backspace returns to the mode list.

File restore uses session snapshots in `rewind_points.jsonl`. Those snapshots cover the file-edit tools. They do not undo shell (`!`) changes or edits you made yourself.

Inline edit-and-resubmit still rewinds conversation only, then resubmits the edited prompt.

## Layout

- Repo: wherever you cloned it (on this Mac, `~/dev/grok-build`)
- Patch branch: `g` (default on the GitHub fork)
- Official branch: `main` (tracks `upstream/main`)
- Command: `~/.local/bin/g`
- Binary: `~/.local/lib/grok-local/xai-grok-pager`
- Daily job: LaunchAgent `ai.x.grok-local-update`
- Job logs: `~/.local/state/grok-local/`

## Scripts

| Script | Purpose |
|---|---|
| `scripts/setup.sh` | New machine: remotes, tools, build `g`, install daily job |
| `scripts/install-grok-local.sh` | Release-build and install `g` |
| `scripts/update-from-upstream.sh` | Fetch `upstream/main`, rebase `g`, rebuild `g`, push `origin` |
| `scripts/install-update-agent.sh` | Generate and load the daily LaunchAgent for this checkout |

Rebuild after a local change:

```bash
~/dev/grok-build/scripts/install-grok-local.sh
```

Pull official changes once, without waiting for the daily job:

```bash
~/dev/grok-build/scripts/update-from-upstream.sh
```

## Daily job

`install-update-agent.sh` writes `~/Library/LaunchAgents/ai.x.grok-local-update.plist` from this machine's `$HOME` and repo path, then bootstraps it. Do not copy a plist from another computer.

- Interval: 24 hours (`StartInterval` 86400).
- `RunAtLoad` is false, so the first automatic run is about a day after the agent was loaded, not immediately.
- It only runs while you are logged in. It will not wake the Mac from sleep to rebase.
- Fetch is HTTPS against public `xai-org/grok-build` (no GitHub auth).
- Push uses whatever `origin` is (HTTPS + `gh`, or SSH).
- A clean rebase rebuilds the `g` command, force-with-lease pushes branch `g` to your fork, and posts a macOS notification.
- A dirty working tree, a rebase conflict, a failed build, or a failed push stops the job and notifies. It does not force-merge.

Status:

```bash
launchctl print "gui/$(id -u)/ai.x.grok-local-update"
```

Logs:

```bash
tail -f ~/.local/state/grok-local/update.log
tail -f ~/.local/state/grok-local/launchd.out.log
tail -f ~/.local/state/grok-local/launchd.err.log
```

Unload:

```bash
launchctl bootout "gui/$(id -u)/ai.x.grok-local-update"
```

Load again:

```bash
~/dev/grok-build/scripts/install-update-agent.sh
```

Skip pushing to GitHub for one run:

```bash
GROK_PUSH_ORIGIN=0 ~/dev/grok-build/scripts/update-from-upstream.sh
```

## Rebase conflicts

Official dumps are large `Synced from monorepo` commits. They often touch rewind files. The job will not resolve those.

```bash
cd ~/dev/grok-build
# fix conflicts, usually:
#   crates/codegen/xai-grok-pager/src/views/rewind.rs
#   crates/codegen/xai-grok-pager/src/app/dispatch/rewind.rs
git add -u
git rebase --continue
./scripts/install-grok-local.sh
git push --force-with-lease origin g
```

Abort instead:

```bash
git rebase --abort
```

Keep the rewind change as one commit on branch `g` so rebases stay small.

## Why not overwrite `grok`

`~/.grok/bin/grok` is a symlink the official updater swaps. If this build lived there, `grok update` and launch auto-update would replace it. The `g` wrapper sets `GROK_DISABLE_AUTOUPDATER=1` for the same reason: a successful official update would restart into `~/.grok/bin/grok` and drop the fork for that session.

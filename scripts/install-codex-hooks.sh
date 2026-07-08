#!/usr/bin/env bash
#
# install-codex-hooks.sh — safely merge agentq's hooks into Codex CLI's
# hooks.json (default ~/.codex/hooks.json, auto-discovered next to config.toml).
#
# Codex uses the same nested hook schema as Claude Code
# ({ "hooks": { "<Event>": [ { "hooks": [ { type, command } ] } ] } }), so this
# mirrors install-claude-hooks.sh but targets hooks.json with Codex events:
#   UserPromptSubmit  -> RUNNING
#   PermissionRequest -> WAITING
#   PostToolUse       -> RUNNING
#   Stop              -> IDLE
#
# Safe by design (same as the Claude installer):
#   * Follows symlinks and writes through to the REAL file (dotfiles-friendly,
#     symlink preserved).
#   * Idempotent — re-running never duplicates a hook, and replaces our own prior
#     hooks even if their command shape changed.
#   * Non-destructive — existing hooks and everything else are preserved.
#   * Backed up, JSON-validated, and written atomically (temp file + rename).
#
# Note: Codex hooks require trust. After installing, the next `codex` launch will
# prompt you to review and trust these hooks — approve them or they won't run.
#
# Usage:
#   scripts/install-codex-hooks.sh [--hooks-file PATH] [--bin PATH] [--dry-run]
#
set -euo pipefail

HOOKS_FILE="${CODEX_HOME:-$HOME/.codex}/hooks.json"
AGENTQ_BIN="${AGENTQ_BIN:-}"
DRY_RUN=0
SHELL_CLEAR=1

log()  { printf '%s\n' "$*" >&2; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die()  { printf 'error: %s\n' "$*" >&2; exit 1; }

usage() {
  cat <<'EOF'
install-codex-hooks.sh — safely merge agentq's hooks into Codex CLI's hooks.json.

Maps UserPromptSubmit -> RUNNING, PermissionRequest -> WAITING,
PostToolUse -> RUNNING, Stop -> IDLE. Follows symlinks, is idempotent, backs up, validates JSON, and
writes atomically. Codex hooks require trust: the next `codex` launch will prompt
you to review and trust them, or they won't run.

Usage:
  scripts/install-codex-hooks.sh [--hooks-file PATH] [--bin PATH] [--dry-run]

  --hooks-file PATH   hooks.json to update (default: $CODEX_HOME/hooks.json,
                      i.e. ~/.codex/hooks.json)
  --bin PATH          path to the agentq binary baked into the hook commands
                      (default: autodetect on PATH, then ~/.cargo/bin/agentq)
  --dry-run, -n       print the diff and exit without writing anything
  --no-shell-clear    skip installing the fish wrapper that clears the pane when
                      interactive codex exits (Codex has no SessionEnd hook)
EOF
}

# --- args ------------------------------------------------------------------
while [ $# -gt 0 ]; do
  case "$1" in
    --hooks-file) HOOKS_FILE="${2:?--hooks-file needs a value}"; shift 2 ;;
    --hooks-file=*) HOOKS_FILE="${1#*=}"; shift ;;
    --bin) AGENTQ_BIN="${2:?--bin needs a value}"; shift 2 ;;
    --bin=*) AGENTQ_BIN="${1#*=}"; shift ;;
    --dry-run|-n) DRY_RUN=1; shift ;;
    --no-shell-clear) SHELL_CLEAR=0; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown argument: $1 (try --help)" ;;
  esac
done

# --- deps ------------------------------------------------------------------
command -v jq >/dev/null 2>&1 || die "jq is required — install it (brew install jq)"

# --- soft feature check ----------------------------------------------------
# `hooks` is a stable, default-on Codex feature, but warn if it's been disabled.
if command -v codex >/dev/null 2>&1; then
  state="$(codex features list 2>/dev/null | awk '$1=="hooks"{print $NF}')"
  if [ -n "$state" ] && [ "$state" != "true" ]; then
    warn "Codex reports the 'hooks' feature is disabled — run: codex features enable hooks"
  fi
fi

# --- locate agentq ---------------------------------------------------------
if [ -z "$AGENTQ_BIN" ]; then
  if command -v agentq >/dev/null 2>&1; then
    AGENTQ_BIN="$(command -v agentq)"
  elif [ -x "${HOME}/.cargo/bin/agentq" ]; then
    AGENTQ_BIN="${HOME}/.cargo/bin/agentq"
  else
    AGENTQ_BIN="agentq"
    warn "agentq not found on PATH or in ~/.cargo/bin; baking the bare name 'agentq'."
    warn "Run 'cargo install --path .' and ensure ~/.cargo/bin is on the hook PATH."
  fi
fi

# --- resolve symlinks to the real target -----------------------------------
resolve_symlink() {
  local p="$1" target
  while [ -L "$p" ]; do
    target="$(readlink "$p")"
    case "$target" in
      /*) p="$target" ;;
      *)  p="$(cd "$(dirname "$p")" && pwd)/$target" ;;
    esac
  done
  printf '%s\n' "$p"
}
REAL="$(resolve_symlink "$HOOKS_FILE")"

log "hooks file : $HOOKS_FILE"
if [ "$REAL" != "$HOOKS_FILE" ]; then
  log "             -> symlink, real file is $REAL (writing through; symlink preserved)"
fi
log "agentq     : $AGENTQ_BIN"

# --- build the hooks we own ------------------------------------------------
# Nested { "hooks": [ { type, command } ] } shape (matcher omitted — optional and
# unused for these non-tool events). Fire-and-forget via `2>/dev/null || true` so
# a missing/failing agentq never disturbs Codex. The binary path is quoted so a
# path with spaces still runs; --type tags the pane's agent kind.
HOOKS="$(jq -n --arg bin "$AGENTQ_BIN" '
  def cmd($st):
    { hooks: [ { type: "command",
                 command: ("\"" + $bin + "\" status " + $st + " --type codex 2>/dev/null || true") } ] };
  # Turn-end topic summary. Fast + non-blocking: `agentq summarize` spawns a
  # detached worker (`claude -p`) and returns at once, computing the pane`s
  # stable @agent_topic once per session.
  def summarize_cmd:
    { hooks: [ { type: "command",
                 command: ("\"" + $bin + "\" summarize --type codex 2>/dev/null || true") } ] };
  {
    UserPromptSubmit:  [ cmd("RUNNING") ],
    PermissionRequest: [ ({ matcher: "" } + cmd("WAITING")) ],
    # PostToolUse fires after any approved tool executes — the only signal that a
    # permission granted in Codex`s own pane was answered (there is no
    # PermissionGranted hook), so it clears a stuck WAITING back to
    # RUNNING without waiting on the watcher.
    PostToolUse:       [ ({ matcher: "" } + cmd("RUNNING")) ],
    Stop:              [ cmd("IDLE"), summarize_cmd ]
  }')"

# --- read + validate current hooks file ------------------------------------
if [ -s "$REAL" ]; then
  current="$(cat "$REAL")"
  printf '%s' "$current" | jq empty 2>/dev/null \
    || die "existing hooks file is not valid JSON, refusing to touch it: $REAL"
else
  current='{}'
fi

# --- merge (reconcile our hooks, keep everything else) ----------------------
# Strip every agentq hook from ALL events, add the current snippet's, and drop
# now-empty events. Idempotent, repairs a prior flat-format install, and cleans
# up hooks under events we no longer use; never touches unrelated hooks.
merged="$(printf '%s' "$current" | jq --argjson snip "$HOOKS" '
  def is_ours:
    [ (.command // empty), ((.hooks // [])[] | .command // empty) ]
    | any(. as $c
          | ($c | test("agentq"))
            and ($c | test("status (RUNNING|WAITING|IDLE|CRASHED|STALLED)|\\bclear\\b|\\bsummarize\\b")));
  .hooks = (.hooks // {})
  | .hooks |= with_entries(.value |= map(select(is_ours | not)))
  | reduce ($snip | to_entries[]) as $e (.;
      .hooks[$e.key] = ((.hooks[$e.key] // []) + $e.value))
  | .hooks |= with_entries(select((.value | length) > 0))
')"

# --- dry run ---------------------------------------------------------------
if [ "$DRY_RUN" -eq 1 ]; then
  log "--- diff (current -> merged), dry run, nothing written ---"
  diff -u \
    <(printf '%s\n' "$current" | jq -S .) \
    <(printf '%s\n' "$merged"  | jq -S .) \
    || true
  exit 0
fi

# --- back up ---------------------------------------------------------------
mkdir -p "$(dirname "$REAL")"
if [ -s "$REAL" ]; then
  backup="${HOOKS_FILE}.bak.$(date +%Y%m%d-%H%M%S)"
  mkdir -p "$(dirname "$backup")"
  cp -p "$REAL" "$backup"
  log "backup     : $backup"
fi

# --- atomic write (temp on same fs as REAL, then rename) -------------------
dir="$(dirname "$REAL")"
tmp="$(mktemp "${dir}/.agentq-hooks.XXXXXX")"
trap 'rm -f "$tmp"' EXIT
printf '%s\n' "$merged" > "$tmp"
jq empty "$tmp" || die "merged result is not valid JSON; aborting without writing"
mv -f "$tmp" "$REAL"
trap - EXIT

# --- verify ----------------------------------------------------------------
ok=1
for pair in "UserPromptSubmit:status RUNNING" \
            "PermissionRequest:status WAITING" \
            "PostToolUse:status RUNNING" \
            "Stop:status IDLE" \
            "Stop:summarize"; do
  ev="${pair%%:*}"; needle="${pair#*:}"
  if ! jq -e --arg ev "$ev" --arg n "$needle" \
        'def cmds: (.command // empty), ((.hooks // [])[] | .command // empty);
         [ (.hooks[$ev] // [])[] | cmds ] | any(contains($n))' "$REAL" >/dev/null; then
    warn "expected hook missing for $ev"
    ok=0
  fi
done
[ "$ok" -eq 1 ] && log "hooks installed for UserPromptSubmit, PermissionRequest, PostToolUse, Stop."

# --- shell wrapper: clear on exit (Codex has no SessionEnd hook) ------------
# A clean codex exit fires no hook, so it can't be caught the way Claude's
# SessionEnd is. Install a fish wrapper that runs `agentq clear` after an
# interactive `codex` session exits. The `status is-interactive` guard keeps
# non-interactive `codex exec` / `codex review` (scripts, other agents) from
# clearing the wrong pane.
if [ "$SHELL_CLEAR" -eq 1 ]; then
  fish_dst="${HOME}/.config/fish/conf.d/agentq-codex.fish"
  mkdir -p "$(dirname "$fish_dst")"
  cat > "$fish_dst" <<'FISH'
# Installed by agentq's install-codex-hooks.sh. Codex has no SessionEnd hook, so
# this wrapper clears the pane's dashboard entry when an interactive session
# exits. `command codex` calls the real binary; the exit code is preserved; the
# `status is-interactive` guard skips non-interactive `codex exec`/`review`.
function codex --wraps codex --description 'codex + agentq clear on exit'
    command codex $argv
    set -l _agentq_exit $status
    if status is-interactive
        agentq clear 2>/dev/null
    end
    return $_agentq_exit
end
FISH
  log "shell wrap : $fish_dst (fish: clears the pane when interactive codex exits)"
  log "             open a new fish session (or 'source $fish_dst') to activate."
fi

log "done — start Codex and approve the hook-trust review, or the hooks won't run."

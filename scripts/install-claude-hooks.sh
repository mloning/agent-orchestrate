#!/usr/bin/env bash
#
# install-claude-hooks.sh — safely merge agentq's hooks into Claude Code's
# settings.json.
#
# Safe by design:
#   * Follows symlinks. ~/.claude/settings.json is commonly a symlink into a
#     dotfiles repo; we resolve it to the REAL file and write THROUGH it, so the
#     dotfiles copy is updated and the symlink itself is preserved (never
#     replaced by a regular file).
#   * Idempotent. Re-running never duplicates a hook, and replaces our own prior
#     hooks even if their command shape changed.
#   * Non-destructive. Existing hooks and all other settings are preserved.
#   * Backed up. The pre-change settings are copied aside before any write.
#   * Validated + atomic. JSON is validated before and after; the new file is
#     written to a temp file on the same filesystem and renamed into place.
#
# Usage:
#   scripts/install-claude-hooks.sh [--settings PATH] [--bin PATH] [--dry-run]
#
#   --settings PATH   settings.json to update (default: ~/.claude/settings.json)
#   --bin PATH        path to the agentq binary baked into the hook commands
#                     (default: autodetect on PATH, then ~/.cargo/bin/agentq)
#   --dry-run, -n     print the diff and exit without writing anything
#
set -euo pipefail

SETTINGS="${HOME}/.claude/settings.json"
AGENTQ_BIN="${AGENTQ_BIN:-}"
DRY_RUN=0

log()  { printf '%s\n' "$*" >&2; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die()  { printf 'error: %s\n' "$*" >&2; exit 1; }

usage() {
  cat <<'EOF'
install-claude-hooks.sh — safely merge agentq's hooks into Claude Code's
settings.json.

Safe by design:
  * Follows symlinks and writes through to the REAL file (so a ~/.claude/
    settings.json symlinked into a dotfiles repo is updated, symlink preserved).
  * Idempotent — re-running never duplicates a hook, and replaces our own prior
    hooks even if their command shape changed.
  * Non-destructive — existing hooks and all other settings are preserved.
  * Backed up, JSON-validated, and written atomically (temp file + rename).

Usage:
  scripts/install-claude-hooks.sh [--settings PATH] [--bin PATH] [--dry-run]

  --settings PATH   settings.json to update (default: ~/.claude/settings.json)
  --bin PATH        path to the agentq binary baked into the hook commands
                    (default: autodetect on PATH, then ~/.cargo/bin/agentq)
  --dry-run, -n     print the diff and exit without writing anything
EOF
}

# --- args ------------------------------------------------------------------
while [ $# -gt 0 ]; do
  case "$1" in
    --settings) SETTINGS="${2:?--settings needs a value}"; shift 2 ;;
    --settings=*) SETTINGS="${1#*=}"; shift ;;
    --bin) AGENTQ_BIN="${2:?--bin needs a value}"; shift 2 ;;
    --bin=*) AGENTQ_BIN="${1#*=}"; shift ;;
    --dry-run|-n) DRY_RUN=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown argument: $1 (try --help)" ;;
  esac
done

# --- deps ------------------------------------------------------------------
command -v jq >/dev/null 2>&1 || die "jq is required — install it (brew install jq)"

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
# Follows a chain of symlinks (absolute or relative) without requiring GNU
# realpath/readlink -f, so it works on stock macOS.
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
REAL="$(resolve_symlink "$SETTINGS")"

log "settings : $SETTINGS"
if [ "$REAL" != "$SETTINGS" ]; then
  log "           -> symlink, real file is $REAL (writing through; symlink preserved)"
fi
log "agentq   : $AGENTQ_BIN"

# --- build the hooks we own ------------------------------------------------
# Claude requires the nested `{ "hooks": [ { type, command } ] }` shape for these
# events — a flat `{ type, command }` entry is rejected by the settings schema.
# The matcher is omitted (optional and silently ignored for non-tool events
# UserPromptSubmit/Notification/Stop).
# Each command is fire-and-forget: `2>/dev/null || true` means a missing or
# failing agentq exits 0 with no output, so it can never block or clutter the
# agent (a Claude hook only blocks on exit code 2). The binary path is quoted
# so a path with spaces still runs; --type tags the pane's agent kind.
# SessionEnd -> `agentq clear` removes the pane from the dashboard when the agent
# exits, so a quit agent doesn't linger as a stale row (its pane lives on as a
# shell, keeping the @agent_* options until they're unset).
HOOKS="$(jq -n --arg bin "$AGENTQ_BIN" '
  def cmd($st):
    { hooks: [ { type: "command",
                 command: ("\"" + $bin + "\" status " + $st + " --type claude 2>/dev/null || true") } ] };
  def clear_cmd:
    { hooks: [ { type: "command",
                 command: ("\"" + $bin + "\" clear 2>/dev/null || true") } ] };
  # Turn-end topic summary. Fast + non-blocking: `agentq summarize` spawns a
  # detached worker (`claude -p`) and returns at once, computing the pane`s
  # stable @agent_topic once per session.
  def summarize_cmd:
    { hooks: [ { type: "command",
                 command: ("\"" + $bin + "\" summarize --type claude 2>/dev/null || true") } ] };
  {
    UserPromptSubmit:  [ cmd("RUNNING") ],
    PermissionRequest: [ ({ matcher: "" } + cmd("WAITING_APPROVAL")) ],
    # PostToolUse fires after any approved tool executes — the only signal that a
    # permission granted in the agent`s own pane was answered (there is no
    # PermissionGranted hook), so it clears a stuck WAITING_APPROVAL back to
    # RUNNING without waiting on the 25s watcher.
    PostToolUse:       [ ({ matcher: "" } + cmd("RUNNING")) ],
    Stop:              [ cmd("IDLE"), summarize_cmd ],
    SessionEnd:        [ clear_cmd ]
  }')"

# --- read + validate current settings --------------------------------------
if [ -s "$REAL" ]; then
  current="$(cat "$REAL")"
  printf '%s' "$current" | jq empty 2>/dev/null \
    || die "existing settings is not valid JSON, refusing to touch it: $REAL"
else
  current='{}'
fi

# --- merge (reconcile our hooks, keep everything else) ----------------------
# Strip every agentq hook from ALL events first (whether stored flat
# {type,command} or nested {hooks:[{command}]}), then add the current snippet's
# and drop any now-empty event. This is idempotent, repairs a prior flat-format
# install, AND cleans up hooks under events we no longer use (e.g. a previous
# Notification mapping) — while never touching the user's unrelated hooks.
merged="$(printf '%s' "$current" | jq --argjson snip "$HOOKS" '
  def is_ours:
    [ (.command // empty), ((.hooks // [])[] | .command // empty) ]
    | any(. as $c
          | ($c | test("agentq"))
            and ($c | test("status (RUNNING|WAITING_APPROVAL|WAITING_INPUT|IDLE|CRASHED|STALLED)|\\bclear\\b|\\bsummarize\\b")));
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
  backup="${SETTINGS}.bak.$(date +%Y%m%d-%H%M%S)"
  mkdir -p "$(dirname "$backup")"
  cp -p "$REAL" "$backup"
  log "backup   : $backup"
fi

# --- atomic write (temp on same fs as REAL, then rename) -------------------
dir="$(dirname "$REAL")"
tmp="$(mktemp "${dir}/.agentq-settings.XXXXXX")"
trap 'rm -f "$tmp"' EXIT
printf '%s\n' "$merged" > "$tmp"
jq empty "$tmp" || die "merged result is not valid JSON; aborting without writing"
mv -f "$tmp" "$REAL"
trap - EXIT

# --- verify ----------------------------------------------------------------
ok=1
for pair in "UserPromptSubmit:status RUNNING" \
            "PermissionRequest:status WAITING_APPROVAL" \
            "PostToolUse:status RUNNING" \
            "Stop:status IDLE" \
            "Stop:summarize" \
            "SessionEnd:clear"; do
  ev="${pair%%:*}"; needle="${pair#*:}"
  if ! jq -e --arg ev "$ev" --arg n "$needle" \
        'def cmds: (.command // empty), ((.hooks // [])[] | .command // empty);
         [ (.hooks[$ev] // [])[] | cmds ] | any(contains($n))' "$REAL" >/dev/null; then
    warn "expected hook missing for $ev"
    ok=0
  fi
done
[ "$ok" -eq 1 ] && log "hooks installed for UserPromptSubmit, PermissionRequest, PostToolUse, Stop, SessionEnd."
log "done — open Claude Code and run /hooks to confirm the merge."

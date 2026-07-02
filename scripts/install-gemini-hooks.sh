#!/usr/bin/env bash
#
# install-gemini-hooks.sh — safely merge agentq's hooks into Gemini CLI's
# settings.json (default ~/.gemini/settings.json).
#
# Gemini CLI uses the same nested hook schema as Claude Code, but its NATIVE
# lifecycle events differ. We use the native ones:
#   BeforeAgent -> RUNNING          (fires when a prompt is submitted)
#   AfterAgent  -> IDLE             (fires when the agent finishes a turn)
#   SessionEnd  -> agentq clear     (removes the agent when the session ends)
#
# WAITING_APPROVAL is intentionally omitted: Gemini has no clean "blocked on
# approval" lifecycle event (BeforeTool fires for every tool, not just blocks),
# so the crash/stall watcher is the backstop for attention there.
#
# Safe by design (same as the Claude/Codex installers): follows symlinks and
# writes through to the real file; idempotent (reconciles our hooks across all
# events); preserves unrelated settings; backs up; validates JSON; atomic write.
#
# Note: agentq keys agents by $TMUX_PANE. If Gemini's hook environment doesn't
# inherit $TMUX_PANE, `agentq status` no-ops (safe) and Gemini agents won't
# register — verify after install.
#
# Usage:
#   scripts/install-gemini-hooks.sh [--settings PATH] [--bin PATH] [--dry-run]
#
set -euo pipefail

SETTINGS="${HOME}/.gemini/settings.json"
AGENTQ_BIN="${AGENTQ_BIN:-}"
DRY_RUN=0

log()  { printf '%s\n' "$*" >&2; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die()  { printf 'error: %s\n' "$*" >&2; exit 1; }

usage() {
  cat <<'EOF'
install-gemini-hooks.sh — safely merge agentq's hooks into Gemini CLI's settings.json.

Maps the native events BeforeAgent -> RUNNING, AfterAgent -> IDLE,
SessionEnd -> clear. Follows symlinks, is idempotent, backs up, validates JSON,
and writes atomically.

Usage:
  scripts/install-gemini-hooks.sh [--settings PATH] [--bin PATH] [--dry-run]

  --settings PATH   settings.json to update (default: ~/.gemini/settings.json)
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
# Gemini entries carry a "matcher" (\"*\" = all) and a "name", per its schema.
HOOKS="$(jq -n --arg bin "$AGENTQ_BIN" '
  def cmd($st):
    { matcher: "*",
      hooks: [ { name: "agentq", type: "command",
                 command: ("\"" + $bin + "\" status " + $st + " --type gemini 2>/dev/null || true") } ] };
  def clear_cmd:
    { matcher: "*",
      hooks: [ { name: "agentq", type: "command",
                 command: ("\"" + $bin + "\" clear 2>/dev/null || true") } ] };
  # Turn-end topic summary. Fast + non-blocking: `agentq summarize` spawns a
  # detached worker (`claude -p`) and returns at once, computing the pane`s
  # stable @agent_topic once per session.
  def summarize_cmd:
    { matcher: "*",
      hooks: [ { name: "agentq", type: "command",
                 command: ("\"" + $bin + "\" summarize --type gemini 2>/dev/null || true") } ] };
  {
    BeforeAgent: [ cmd("RUNNING") ],
    AfterAgent:  [ cmd("IDLE"), summarize_cmd ],
    SessionEnd:  [ clear_cmd ]
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
merged="$(printf '%s' "$current" | jq --argjson snip "$HOOKS" '
  def is_ours:
    [ (.command // empty), ((.hooks // [])[] | .command // empty) ]
    | any(. as $c
          | ($c | test("agentq"))
            and ($c | test("status (RUNNING|WAITING_APPROVAL|IDLE|CRASHED|STALLED)|\\bclear\\b|\\bsummarize\\b")));
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

# --- atomic write ----------------------------------------------------------
dir="$(dirname "$REAL")"
tmp="$(mktemp "${dir}/.agentq-gemini.XXXXXX")"
trap 'rm -f "$tmp"' EXIT
printf '%s\n' "$merged" > "$tmp"
jq empty "$tmp" || die "merged result is not valid JSON; aborting without writing"
mv -f "$tmp" "$REAL"
trap - EXIT

# --- verify ----------------------------------------------------------------
ok=1
for pair in "BeforeAgent:status RUNNING" \
            "AfterAgent:status IDLE" \
            "AfterAgent:summarize" \
            "SessionEnd:clear"; do
  ev="${pair%%:*}"; needle="${pair#*:}"
  if ! jq -e --arg ev "$ev" --arg n "$needle" \
        'def cmds: (.command // empty), ((.hooks // [])[] | .command // empty);
         [ (.hooks[$ev] // [])[] | cmds ] | any(contains($n))' "$REAL" >/dev/null; then
    warn "expected hook missing for $ev"
    ok=0
  fi
done
[ "$ok" -eq 1 ] && log "hooks installed for BeforeAgent, AfterAgent, SessionEnd."
log "done — restart Gemini CLI to pick up the hooks."

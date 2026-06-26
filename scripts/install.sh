#!/usr/bin/env bash
#
# install.sh — set up agentq integration for every agent you have installed,
# safely. Delegates to the per-agent installers, each of which is independently
# safe (follows symlinks, backs up, validates JSON, idempotent, atomic write):
#
#   claude  -> install-claude-hooks.sh   (UserPromptSubmit/PermissionRequest/Stop/SessionEnd)
#   codex   -> install-codex-hooks.sh    (hooks.json + fish clear-on-exit wrapper)
#   gemini  -> install-gemini-hooks.sh   (BeforeAgent/AfterAgent/SessionEnd)
#   tmux    -> install-tmux.sh           (prefix+i dashboard keybinding)
#
# Only installs for tools whose CLI is on PATH; others are reported and skipped.
# Nothing is written for an agent you don't have. Pass --dry-run to preview all.
#
# Usage:
#   scripts/install.sh [--dry-run] [--bin PATH] [--skip claude,codex,gemini]
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
DRY_RUN=0
SKIP=""
BIN_ARG=()

log()  { printf '%s\n' "$*" >&2; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die()  { printf 'error: %s\n' "$*" >&2; exit 1; }

usage() {
  cat <<'EOF'
install.sh — install agentq hooks for every agent CLI you have.

Runs the per-agent installers (claude, codex, gemini) for whichever CLIs are on
PATH; codex also gets the fish clear-on-exit wrapper. Each installer is safe:
follows symlinks, backs up, validates JSON, idempotent, atomic write.

Usage:
  scripts/install.sh [--dry-run] [--bin PATH] [--skip a,b]

  --dry-run, -n   preview every change (no writes), passed to each installer
  --bin PATH      agentq binary to bake into the hook commands (default: autodetect)
  --skip LIST     comma-separated agents to skip, e.g. --skip gemini
EOF
}

# --- args ------------------------------------------------------------------
while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run|-n) DRY_RUN=1; shift ;;
    --bin) BIN_ARG=(--bin "${2:?--bin needs a value}"); shift 2 ;;
    --bin=*) BIN_ARG=(--bin "${1#*=}"); shift ;;
    --skip) SKIP="${2:?--skip needs a value}"; shift 2 ;;
    --skip=*) SKIP="${1#*=}"; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown argument: $1 (try --help)" ;;
  esac
done

# --- preflight -------------------------------------------------------------
command -v jq >/dev/null 2>&1 || die "jq is required — install it (brew install jq)"
if ! command -v agentq >/dev/null 2>&1 && [ ! -x "${HOME}/.cargo/bin/agentq" ]; then
  warn "agentq not found on PATH or in ~/.cargo/bin — the hooks will call a binary"
  warn "that doesn't exist yet. Run 'cargo install --path .' to build it."
fi

# Pass-through args for each sub-installer (guard empty arrays under set -u).
pass=()
[ "$DRY_RUN" -eq 1 ] && pass+=(--dry-run)
pass+=(${BIN_ARG[@]+"${BIN_ARG[@]}"})

DONE=""
SKIPPED=""
FAILED=""

run_agent() {
  agent="$1"; cli="$2"; script="$3"
  case ",$SKIP," in
    *",$agent,"*) log "• $agent — skipped (--skip)"; SKIPPED="$SKIPPED $agent"; return ;;
  esac
  if ! command -v "$cli" >/dev/null 2>&1; then
    log "• $agent — skipped ('$cli' not on PATH)"
    SKIPPED="$SKIPPED $agent"
    return
  fi
  if [ ! -x "$SCRIPT_DIR/$script" ]; then
    warn "$agent — installer not found/executable: $SCRIPT_DIR/$script"
    FAILED="$FAILED $agent"
    return
  fi
  log ""
  log "── $agent ───────────────────────────────────────────"
  if "$SCRIPT_DIR/$script" ${pass[@]+"${pass[@]}"}; then
    DONE="$DONE $agent"
  else
    warn "$agent installer exited non-zero"
    FAILED="$FAILED $agent"
  fi
}

run_agent claude claude install-claude-hooks.sh
run_agent codex  codex  install-codex-hooks.sh
run_agent gemini gemini install-gemini-hooks.sh
run_agent tmux   tmux   install-tmux.sh

# --- summary ---------------------------------------------------------------
log ""
log "── summary ──────────────────────────────────────────"
[ -n "$DONE" ]    && log "installed:${DONE}"
[ -n "$SKIPPED" ] && log "skipped:  ${SKIPPED}"
[ -n "$FAILED" ]  && log "failed:   ${FAILED}"
if [ "$DRY_RUN" -eq 1 ]; then
  log "(dry run — nothing was written)"
else
  log "reminders: Claude → run /hooks; Codex → approve hook-trust on next launch +"
  log "open a new fish session; Gemini → restart the CLI."
fi
[ -z "$FAILED" ] || exit 1

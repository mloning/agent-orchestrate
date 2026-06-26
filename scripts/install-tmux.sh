#!/usr/bin/env bash
#
# install-tmux.sh — add the agentq dashboard keybinding to ~/.tmux.conf safely.
#
# Inlines a marker-wrapped `bind-key <key> run-shell "<agentq> open"` using the
# resolved ABSOLUTE agentq path, so tmux's run-shell PATH doesn't matter (the
# common reason a bare `agentq` binding silently fails). Safe by design:
#   * Follows symlinks and writes through to the REAL file (dotfiles-friendly).
#   * Idempotent — re-running replaces our marked block, never duplicates it,
#     and never touches the rest of your tmux.conf.
#   * Backed up before any write; written atomically (temp + rename).
#   * Applied live to a running tmux server so prefix+<key> works immediately.
#
# Usage:
#   scripts/install-tmux.sh [--tmux-conf PATH] [--bin PATH] [--key i] [--dry-run]
#
set -euo pipefail

TMUX_CONF="${HOME}/.tmux.conf"
AGENTQ_BIN="${AGENTQ_BIN:-}"
KEY="i"
DRY_RUN=0
BEGIN_MARK="# >>> agentq (managed by install-tmux.sh) >>>"
END_MARK="# <<< agentq <<<"

log()  { printf '%s\n' "$*" >&2; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die()  { printf 'error: %s\n' "$*" >&2; exit 1; }

usage() {
  cat <<'EOF'
install-tmux.sh — add the agentq dashboard keybinding to ~/.tmux.conf.

Inlines `bind-key <key> run-shell "<absolute agentq> open"` in a marked block.
Follows symlinks, backs up, idempotent, atomic, and applies it live.

Usage:
  scripts/install-tmux.sh [--tmux-conf PATH] [--bin PATH] [--key i] [--dry-run]

  --tmux-conf PATH  tmux config to update (default: ~/.tmux.conf)
  --bin PATH        agentq binary for the binding (default: autodetect)
  --key KEY         prefix key to bind (default: i)
  --dry-run, -n     show the block and exit without writing
EOF
}

# --- args ------------------------------------------------------------------
while [ $# -gt 0 ]; do
  case "$1" in
    --tmux-conf) TMUX_CONF="${2:?--tmux-conf needs a value}"; shift 2 ;;
    --tmux-conf=*) TMUX_CONF="${1#*=}"; shift ;;
    --bin) AGENTQ_BIN="${2:?--bin needs a value}"; shift 2 ;;
    --bin=*) AGENTQ_BIN="${1#*=}"; shift ;;
    --key) KEY="${2:?--key needs a value}"; shift 2 ;;
    --key=*) KEY="${1#*=}"; shift ;;
    --dry-run|-n) DRY_RUN=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown argument: $1 (try --help)" ;;
  esac
done

# --- locate agentq ---------------------------------------------------------
if [ -z "$AGENTQ_BIN" ]; then
  if command -v agentq >/dev/null 2>&1; then
    AGENTQ_BIN="$(command -v agentq)"
  elif [ -x "${HOME}/.cargo/bin/agentq" ]; then
    AGENTQ_BIN="${HOME}/.cargo/bin/agentq"
  else
    AGENTQ_BIN="agentq"
    warn "agentq not found on PATH or in ~/.cargo/bin; using the bare name 'agentq'."
    warn "tmux's run-shell may not have ~/.cargo/bin on PATH — install agentq and re-run."
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
REAL="$(resolve_symlink "$TMUX_CONF")"

log "tmux conf : $TMUX_CONF"
if [ "$REAL" != "$TMUX_CONF" ]; then
  log "            -> symlink, real file is $REAL (writing through; symlink preserved)"
fi
log "agentq    : $AGENTQ_BIN"

block="$BEGIN_MARK
# agentq triage dashboard: prefix + $KEY toggles the dashboard
bind-key $KEY run-shell \"$AGENTQ_BIN open\"
$END_MARK"

# Current content with any prior agentq block stripped (exact marker match).
if [ -f "$REAL" ]; then
  current="$(cat "$REAL")"
else
  current=""
fi
stripped="$(printf '%s\n' "$current" | awk -v b="$BEGIN_MARK" -v e="$END_MARK" '
  $0==b {inb=1}
  !inb  {print}
  $0==e {inb=0}
')"

# --- dry run ---------------------------------------------------------------
if [ "$DRY_RUN" -eq 1 ]; then
  log "--- would ensure this block in $REAL (dry run) ---"
  printf '%s\n' "$block" >&2
  exit 0
fi

# --- back up ---------------------------------------------------------------
mkdir -p "$(dirname "$REAL")"
if [ -f "$REAL" ]; then
  backup="${TMUX_CONF}.bak.$(date +%Y%m%d-%H%M%S)"
  mkdir -p "$(dirname "$backup")"
  cp -p "$REAL" "$backup"
  log "backup    : $backup"
fi

# --- atomic write (strip old block, append fresh) --------------------------
dir="$(dirname "$REAL")"
tmp="$(mktemp "${dir}/.agentq-tmux.XXXXXX")"
trap 'rm -f "$tmp"' EXIT
{
  if [ -n "$stripped" ]; then printf '%s\n' "$stripped"; fi
  printf '%s\n' "$block"
} > "$tmp"
mv -f "$tmp" "$REAL"
trap - EXIT
log "binding   : prefix + $KEY → $AGENTQ_BIN open"

# --- apply live to a running server (best effort) --------------------------
if command -v tmux >/dev/null 2>&1 && tmux info >/dev/null 2>&1; then
  if tmux bind-key "$KEY" run-shell "$AGENTQ_BIN open" 2>/dev/null; then
    log "applied live to the running tmux server."
  else
    log "reload tmux to apply (tmux source-file $TMUX_CONF)."
  fi
else
  log "no running tmux server — the binding loads next time tmux starts."
fi

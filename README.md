# agentq — Agent Triage Dashboard

A persistent, live-updating tmux dashboard for triaging AI coding agents
(Claude Code, Codex CLI, Gemini agy CLI) running in parallel tmux panes.

## What it does

- **Fleet visibility** — shows all registered agents with type, status, location, age, and message
- **Priority sorting** — attention-states (CRASHED, WAITING_APPROVAL, WAITING_INPUT) float to the top
- **Live updates** — polls tmux state every ~500ms, no manual refresh needed
- **Act in place** — approve (`y`), deny (`n`), or free-text reply (`r`) without leaving the dashboard
- **Exact-pane warp** — `Enter` jumps to the agent's precise tmux pane; `prefix+i` returns
- **Persistent** — runs in its own tmux session, survives navigation, always current

## Quick start

```bash
# Build and install (puts `agentq` in ~/.cargo/bin)
cargo install --path .

# Wire everything for every agent CLI you have — one safe command:
#   Claude + Codex (+ its fish clear-on-exit wrapper) + Gemini hooks, plus the
#   tmux `prefix + i` keybinding. Each merge follows symlinks (dotfiles repos),
#   backs up, validates, and is idempotent.
# Preview first, then apply:
scripts/install.sh --dry-run
scripts/install.sh
# Per-tool installers also exist: install-{claude,codex,gemini}-hooks.sh, install-tmux.sh
#
# After: Claude → run /hooks · Codex → approve the hook-trust review on next
# launch + open a new fish session · Gemini → restart the CLI. (tmux is applied live.)
```

The hook commands pass the agent kind with `--type` and use the absolute `agentq`
path, so nothing extra needs to be exported per pane. Each command ends in
`2>/dev/null || true` so a missing or failing `agentq` exits cleanly and never
blocks or clutters the agent. (Claude delivers hook input as stdin JSON, not env
vars, so the status messages are short static labels.)

## Usage

```bash
# Open the dashboard (or toggle back if already viewing it)
# Bound to: prefix + i
agentq open

# Inside the dashboard:
#   j/k or arrows  — navigate agents
#   y              — approve (sends y⏎ to agent)
#   n              — deny (sends n⏎ to agent)
#   r              — reply with free-text (guarded for non-waiting agents)
#   Enter          — warp to agent's exact pane
#   x              — clear the selected agent from the dashboard (stale rows)
#   d              — toggle the live session preview (bottom half, on by default)
#   q              — return to previous pane

# Manually set agent status (used by hooks, not typically called directly)
agentq status WAITING_APPROVAL "permission requested"
agentq status RUNNING "working on task"
agentq status IDLE "finished"
```

## Architecture

```
Agent pane (hook fires)     →  agentq status  →  tmux pane user-options (@agent_*)
                                                         ↑
Dashboard (agentq tui)     ←  tmux list-panes (poll)  ───┘
                                                         
prefix+i  →  agentq open  →  toggle between work and dashboard session
```

**tmux IS the database.** Agent state is stored as tmux pane user-options (`@agent_status`,
`@agent_type`, `@agent_msg`, `@agent_updated`). No SQLite, no separate daemon, no external state.

**How agents are cleared.** A row disappears when its pane's options are gone. That happens
three ways: (1) the pane is destroyed (tmux drops its options); (2) the agent exits and runs
`agentq clear` — Claude via its `SessionEnd` hook, Codex via a fish wrapper the installer drops
in (`~/.config/fish/conf.d/agentq-codex.fish`, since Codex has no session-end hook) that clears
the pane when an interactive `codex` exits; (3) you press `x` in the dashboard to clear a row
manually. The `watch` crash detector is the backstop for hard kills. A still-live agent that
gets cleared re-registers on its next hook.

## Subcommands

| Command | Description |
|---------|-------------|
| `agentq status <STATUS> [msg]` | Hook target — tags the current pane with status |
| `agentq tui` | Launch the persistent TUI dashboard |
| `agentq open` | Summon/toggle the dashboard (bind to `prefix+i`) |
| `agentq watch` | Crash/stall detection loop (Phase 2) |

## Agent states (priority order, highest first)

| State | Tier | Color | Meaning |
|-------|------|-------|---------|
| CRASHED | 0 | Red | Agent died to bare shell |
| STALLED | 0 | Red | No progress past threshold |
| WAITING_APPROVAL | 1 | Yellow | Blocked on y/n prompt |
| WAITING_INPUT | 2 | Magenta | Blocked on free-text prompt |
| RUNNING | 3 | Green | Actively working |
| IDLE | 4 | Gray | Finished, awaiting next prompt |

## Project structure

```
├── Cargo.toml
├── src/
│   ├── main.rs          # CLI dispatch (clap)
│   ├── model.rs         # Status enum, Agent struct, priority ordering
│   ├── tmux.rs          # All tmux interaction (the only module that shells out)
│   ├── status.rs        # `agentq status` — hook target (fast path)
│   ├── tui.rs           # ratatui dashboard — the main UI
│   ├── open.rs          # `agentq open` — summon/toggle logic
│   └── watch.rs         # `agentq watch` — crash/stall detection (Phase 2)
├── settings/            # Hook configuration snippets
├── tmux/                # tmux keybinding config
├── launchd/             # macOS launchd plist (Phase 2)
└── README.md
```

## Requirements

- Rust 1.70+
- tmux 3.2+ (for pane user-options)
- macOS (tested on darwin with fish shell)

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
# Build and install
cargo install --path .

# Wire tmux keybinding (add to ~/.tmux.conf)
source-file ~/Dev/projects/agent-orchestrate/tmux/agent-orchestrate.conf
# Then reload: tmux source ~/.tmux.conf

# Wire Claude Code hooks (merge into ~/.claude/settings.json)
# See settings/claude-settings.snippet.json

# Wire Codex hooks (merge into ~/.codex/config.toml)
# See settings/codex-hooks.snippet.toml

# Set agent type in your shell environment (per-agent pane)
export AGENTQ_TYPE=claude  # or codex, gemini
```

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
#   d              — toggle detail pane (captured output)
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
`@agent_type`, `@agent_msg`, `@agent_updated`). When a pane dies, its state auto-cleans.
No SQLite, no separate daemon, no external state.

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

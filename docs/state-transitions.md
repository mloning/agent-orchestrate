# Agent state transitions

How `agentq` tracks each agent's state, and how the TUI becomes aware of changes.

## Architecture: no event bus — everything goes through tmux pane options

The single source of truth is a set of **pane-scoped tmux options** on each agent's
pane: `@agent_status`, `@agent_msg`, `@agent_updated`, `@agent_type`, `@agent_topic`
(`tmux.rs` `set_status`). Three things *write* them and the TUI *reads* them:

- **Hooks** — the agent fires a shell hook that runs `agentq status <STATE>` →
  `status::run` → `tmux::set_status` (`status.rs`).
- **The watcher** (`agentq watch`, a launchd daemon) — scans every pane every **25s**
  and *overrides* the status based on what's on screen (`watch.rs` `scan_once`). This is
  detection-only; it never registers a new pane.
- **The TUI itself** — does *optimistic* writes when you act inside it (`tui.rs`
  `act_yes_no`, `send_reply`).

The TUI has **no event subscription**. `run_loop` polls `tmux list-panes -a` every
**500ms** (`POLL_INTERVAL`, `tui.rs`), re-parses every row, re-sorts, and re-renders
(`refresh`). So the TUI lag to *see* a change is ≤500ms for hook-driven changes, and
≤~25s for watcher-driven ones.

## Transition-by-transition

The **To** column carries an italic note when several rows share the same From→To but
differ by trigger.

| From | To | Claude | Codex | Mechanism |
|---|---|---|---|---|
| idle | running | `UserPromptSubmit` → `RUNNING` | same | hook |
| running | idle | `Stop` → `IDLE` | same | hook |
| running | waiting approval *(tool permission)* | `PermissionRequest` → `WAITING_APPROVAL` | same | hook |
| running | waiting approval *(plan prompt)* | **watcher** matches on-screen prompt text (`is_active_plan_prompt`), sets `WAITING_APPROVAL` msg `"plan approval"` | n/a | watcher only — Claude fires *no* hook for plan prompts (issue #19283) |
| waiting approval | running *(approved in TUI · `y`)* | TUI optimistically sets `RUNNING` immediately | same | TUI write |
| waiting approval | running *(approved in the pane)* | `PostToolUse` → `RUNNING` fires after the approved tool executes; watcher's "esc to interrupt" check (`looks_working`) is the fallback | same — Codex also fires `PostToolUse` | hook, watcher fallback — see below |
| waiting approval | running *(denied in TUI · `n`)* | optimistic `RUNNING`, then real status corrects | same | TUI write |
| running | stalled | n/a hook — watcher: `RUNNING` + no output change for 600s | same | watcher only |
| any | crashed | watcher: chrome gone + crash sig in live region or bare shell (`looks_crashed`) | same | watcher only |
| any | removed | `SessionEnd` → `agentq clear` | **fish wrapper** runs `agentq clear` on `codex` exit (no SessionEnd hook) — `codex-clear.fish` | hook / shell shim |

## The "waiting for approval" stuck-after-approving-in-the-pane gap

**Claude Code fires no hook when a permission is *granted*.** The lifecycle is
`PermissionRequest → [user approves] → tool executes → PostToolUse`. There is no
`PermissionGranted` event. And the dashboard's optimistic `RUNNING` only happens when
you press `y`/`n`/`r` **inside the TUI** — when you instead hit `Enter` to warp and
approve in the agent's own pane, nothing writes `RUNNING`.

Previously the row then stayed `WAITING_APPROVAL` until one of two fallbacks:

1. The watcher's next scan (≤25s) happened to catch the **"esc to interrupt"** footer in
   the live region → `RUNNING`. But for a *real* `PermissionRequest` (message is the
   project name, not `"plan approval"`), the `answered_plan` branch never applies — so
   if the approved action was quick or quiet and the interrupt footer wasn't on screen at
   scan time, the watcher missed it.
2. The turn eventually ended → `Stop` → `IDLE`.

Net effect: a window of up to ~25s (sometimes lasting until the turn ended) where the row
wrongly showed `WAITING_APPROVAL`.

### Fix (implemented)

The complement hook that *does* fire is now wired: **`PostToolUse` → `agentq status
RUNNING`** (`settings/claude-settings.snippet.json`, installed by
`scripts/install-claude-hooks.sh`). `PostToolUse` fires immediately after any approved
tool executes (and after every tool generally — which is correct, the agent *is* running
while using tools), so it clears `WAITING_APPROVAL` within ~500ms instead of relying on
the 25s watcher heuristic:

```
RUNNING → WAITING_APPROVAL (PermissionRequest) → RUNNING (PostToolUse) → IDLE (Stop)
```

Codex supports the same `PostToolUse` event (verified against the OpenAI Codex hooks docs
and the codex 0.142.4 binary), so the fix is wired for both: `settings/*-hooks.snippet.*`
and both installers (`scripts/install-{claude,codex}-hooks.sh`).

Remaining caveats:

- This covers **tool-permission** approvals. Claude **plan** approvals still fire no hook
  and keep relying on the watcher — unavoidable, and already handled.
- For Codex, the watcher's `looks_working` ("Esc to interrupt") check remains the
  fallback for any case where `PostToolUse` doesn't fire (e.g. an approval that runs no
  subsequent tool).

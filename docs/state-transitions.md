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
| running | waiting approval *(tool permission)* | `PermissionRequest` → `WAITING`; **watcher fallback** matches the on-screen prompt (`is_active_tool_prompt`, msg `"tool approval"`) for variants that fire no hook — notably the Bash command-safety / "brace expansion" confirmation | same (hook only) | hook + watcher fallback |
| running | waiting approval *(plan prompt)* | **watcher** matches on-screen prompt text (`is_active_plan_prompt`), sets `WAITING` msg `"plan approval"` | n/a | watcher only — Claude fires *no* hook for plan prompts (issue #19283) |
| waiting approval | running *(approved in TUI · `y`)* | TUI optimistically sets `RUNNING` immediately | same | TUI write |
| waiting approval | running *(approved in the pane)* | `PostToolUse` → `RUNNING` fires after the approved tool executes; watcher's "esc to interrupt" check (`looks_working`) is the fallback | same — Codex also fires `PostToolUse` | hook, watcher fallback — see below |
| waiting approval | running *(denied in TUI · `n`)* | optimistic `RUNNING`, then real status corrects | same | TUI write |
| running | stalled *(no progress)* | n/a hook — watcher: `RUNNING` + no output change for 600s | same | watcher only |
| any | stalled *(crashed to shell)* | watcher: chrome gone + crash sig in live region or bare shell (`looks_crashed`) | same | watcher only |
| running | stalled *(dead turn)* | watcher: `API Error:` in live region, turn ended (`looks_errored`) — Claude only | n/a | watcher only |
| any | removed | `SessionEnd` → `agentq clear` | **fish wrapper** runs `agentq clear` on `codex` exit (no SessionEnd hook) — `codex-clear.fish` | hook / shell shim |

## The "waiting for approval" stuck-after-approving-in-the-pane gap

**Claude Code fires no hook when a permission is *granted*.** The lifecycle is
`PermissionRequest → [user approves] → tool executes → PostToolUse`. There is no
`PermissionGranted` event. And the dashboard's optimistic `RUNNING` only happens when
you press `y`/`n`/`r` **inside the TUI** — when you instead hit `Enter` to warp and
approve in the agent's own pane, nothing writes `RUNNING`.

Previously the row then stayed `WAITING` until one of two fallbacks:

1. The watcher's next scan (≤25s) happened to catch the **"esc to interrupt"** footer in
   the live region → `RUNNING`. But for a *real* `PermissionRequest` (message is the
   project name, not `"plan approval"`), the `answered_plan` branch never applies — so
   if the approved action was quick or quiet and the interrupt footer wasn't on screen at
   scan time, the watcher missed it.
2. The turn eventually ended → `Stop` → `IDLE`.

Net effect: a window of up to ~25s (sometimes lasting until the turn ended) where the row
wrongly showed `WAITING`.

### Fix (implemented)

The complement hook that *does* fire is now wired: **`PostToolUse` → `agentq status
RUNNING`** (`settings/claude-settings.snippet.json`, installed by
`scripts/install-claude-hooks.sh`). `PostToolUse` fires immediately after any approved
tool executes (and after every tool generally — which is correct, the agent *is* running
while using tools), so it clears `WAITING` within ~500ms instead of relying on
the 25s watcher heuristic:

```
RUNNING → WAITING (PermissionRequest) → RUNNING (PostToolUse) → IDLE (Stop)
```

Codex supports the same `PostToolUse` event (verified against the OpenAI Codex hooks docs
and the codex 0.142.4 binary), so the fix is wired for both: `settings/*-hooks.snippet.*`
and both installers (`scripts/install-{claude,codex}-hooks.sh`).

Remaining caveats:

- This covers **tool-permission** approvals. Claude **plan** approvals still fire no hook
  and keep relying on the watcher — unavoidable, and already handled.
- Not every tool-permission prompt fires `PermissionRequest`. The Bash command-safety
  confirmation (the "brace expansion" / command-injection warning) is a secondary gate
  that was observed to leave the row stuck at `RUNNING`. The watcher now scrapes for the
  live tool-permission prompt (`is_active_tool_prompt`, "Do you want to proceed?") as a
  fallback and auto-resumes it (msg `"tool approval"`) once the prompt clears — mirroring
  the plan-prompt path. Cost: up to ~25s of watcher lag before an un-hooked prompt shows.
- The watcher scopes prompt detection to the pane's **live region** (bottom
  `LIVE_REGION_LINES`). When a **task list** is active, Claude pins its panel (an
  `N tasks (…)` header plus `◼`/`◻` items) to the very bottom of the pane — *below* the
  prompt box — pushing the prompt's markers out of that window. That left an un-hooked
  prompt (or any plan prompt) undetected, so the row stayed `RUNNING` and dropped out of
  the attention tier instead of showing `WAITING`. `live_region` now peels a trailing task
  panel off first (`trim_trailing_task_panel`), so the prompt lands back in the live region.
  The same window is also blown by the prompt's own **`(ctrl+b … to run in background)`
  hint footer** — several such lines can render below `Esc to cancel …` (observed as four
  under a 5-agent run), again pushing `Do you want to proceed?` out. `live_region` peels
  that footer too (`trim_trailing_bg_hints`), looping until both chrome blocks are gone.
- For Codex, the watcher's `looks_working` ("Esc to interrupt") check remains the
  fallback for any case where `PostToolUse` doesn't fire (e.g. an approval that runs no
  subsequent tool).

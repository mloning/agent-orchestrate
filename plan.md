# Blueprint: Hybrid Agent Orchestration Queue

A local, high-performance Human-in-the-Loop (HITL) priority queue built with **Rust**, **SQLite**, and **tmux**. This architecture combines the efficiency of **event-driven hooks** for instant notifications with a **background watchdog** to eliminate silent agent crashes, alongside a **remote cockpit** for fast macro-approvals.

---

## 1. Core Architecture & Data State

The entire state of your active agent fleet lives inside a local SQLite database operating in **Write-Ahead Logging (WAL) mode** for safe concurrent access by multiple parallel agents.

### Database Schema (`~/.agent_queue.db`)

```sql
CREATE TABLE IF NOT EXISTS agent_queue (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_target TEXT UNIQUE, -- Format: "session_name:window_index"
    agent_type TEXT,            -- "claude" | "codex"
    status TEXT,                -- "WAITING_APPROVAL" | "IDLE" | "CRASHED" | "STALLED"
    message TEXT,
    updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
);
```

---

## 2. Component Blueprint

### A. The Producer Layer: Event Hooks

Triggered natively at the binary runtime level by your agents. They fire the sub-millisecond Rust binary (`agent-q push`) to update the SQLite database.

* **Claude Code (`~/.claude/settings.json`):** Maps to `permission_prompt`, `idle_prompt`, and `elicitation_dialog`.
* **Codex CLI (`~/.codex/hooks.json` or `config.toml`):** Maps to `PermissionRequest` and `Stop`.

### B. The Resiliency Layer: Watchdog Daemon (`agent-qd`)

A lightweight background loop script (or compiled Rust thread) running every 20-30 seconds to catch silent failures and handle loop guardrails.

1.  **Crash Detection:** Executes `tmux capture-pane -p -t "$TARGET"` to parse terminal output for node path stack-traces or raw shell prompts (`$`, `❯`), indicating the agent crashed out to the bare shell.
2.  **Stall Detection:** Compares the current time against the execution timestamp. If an agent remains active with no lifecycle updates for more than 10 minutes, it flags the target as `[STALLED]` and pushes it to the top of the queue.

### C. The Consumer Layer: Remote Cockpit TUI

A global tmux keybinding (`prefix + i`) spawns an interactive floating popup window running an `fzf` screen or an ultra-lean Rust terminal UI (`ratatui`) mapped directly to the SQLite data state.

---

## 3. Interaction & Context-Switching Matrix

Your dashboard handles triage dynamically based on your keystrokes, maximizing focus by letting you choose whether a context switch is truly necessary.

| Keypress | Intended Action | Background Mechanism |
| :--- | :--- | :--- |
| **`y` / `n`** | **Remote Macro-Approval** | Executes `tmux send-keys -t "target" "y" Enter`, updates SQLite state, and keeps you in your current window. |
| **`Enter`** | **Full Context Switch** | Executes `tmux switch-client -t "target"`, physically warping your entire terminal viewport directly into that agent's repo, worktree, and active Neovim session. |
| **`Esc`** | **Dismiss Dashboard** | Closes the floating tmux popup instantly, returning you exactly to the line of code you were writing. |

---

## 4. Key Advantages over Out-of-the-Box Tooling

* **Zero Polling CPU Bloat:** The primary mechanism is purely push-based via native application hooks. The heavy lifting only happens when state changes.
* **Token Isolation:** No system prompt padding is required to instruct the LLM on how to report its status; orchestration is fully abstracted to the infrastructure layer.
* **State Deduplication:** The SQL `UNIQUE` constraint on `session_target` prevents an agent caught in an automated retry loop from spamming your notification deck.

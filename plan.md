# Blueprint: Hybrid Agent Orchestration Queue

A local, high-performance Human-in-the-Loop (HITL) priority queue built natively with **Bash**, **JSONL**, **fzf**, and **tmux**. This architecture combines the efficiency of **event-driven hooks** for instant notifications with a **background watchdog** to catch silent agent crashes, alongside a **remote cockpit** for fast macro-approvals without context switching.

---

## 1. Core Architecture & Data State

The entire state of your active agent fleet lives inside a local JSON Lines (JSONL) file, guarded by file locks (`flock`) to ensure safe concurrent access by multiple parallel agents.

### Data State (`~/.agent_queue.jsonl`)

State is stored as newline-delimited JSON objects to allow rapid parsing with `jq`.

```json
{"session_target": "claude:1", "agent_type": "claude", "status": "WAITING_APPROVAL", "message": "Proceed with modifying index.ts?", "updated_at": "2026-06-22T20:55:00Z"}
{"session_target": "codex:2", "agent_type": "codex", "status": "CRASHED", "message": "Exited to shell prompt", "updated_at": "2026-06-22T20:56:00Z"}
```

---

## 2. Component Blueprint

### A. The Producer Layer: Event Hooks (`agent-q push`)

Triggered natively by your agents. They execute a fast shell script that uses `flock` to append or update their state in the JSONL file.

* **Claude Code (`~/.claude/settings.json`):** Maps to `permission_prompt`, `idle_prompt`, and `elicitation_dialog`.
* **Codex CLI (`~/.codex/hooks.json` or `config.toml`):** Maps to `PermissionRequest` and `Stop`.

### B. The Resiliency Layer: Watchdog Daemon (`agent-qd`)

A lightweight background bash `while` loop running every 20-30 seconds to catch silent failures and handle loop guardrails.

1.  **Crash Detection:** Executes `tmux capture-pane -p -t "$TARGET"` to parse terminal output for node path stack-traces or raw shell prompts (`$`, `❯`), indicating the agent crashed out to the bare shell.
2.  **Stall Detection:** Compares the current time against the execution timestamp using `date`. If an agent remains active with no lifecycle updates for more than 10 minutes, it flags the target as `[STALLED]` in the `.jsonl` file.

### C. The Consumer Layer: Remote Cockpit TUI (`fzf`)

A global tmux keybinding (`prefix + i`) spawns an interactive floating popup window running a pipeline of `cat ~/.agent_queue.jsonl | jq | fzf`.

---

## 3. Interaction & Context-Switching Matrix

Your dashboard handles triage dynamically based on your keystrokes inside `fzf`, maximizing focus by letting you choose whether a context switch is truly necessary.

| Keypress | Intended Action | Background Mechanism |
| :--- | :--- | :--- |
| **`y` / `n`** | **Remote Macro-Approval** | Handled via `fzf --bind`. Executes `tmux send-keys -t "target" "y" Enter`, updates the JSONL state, and keeps you in your current window. |
| **`Enter`** | **Full Context Switch** | Handled via `fzf` default action. Executes `tmux switch-client -t "target"`, physically warping your entire terminal viewport directly into that agent's pane. |
| **`Esc`** | **Dismiss Dashboard** | Closes the floating tmux popup instantly, returning you exactly to where you were. |

---

## 4. Key Advantages over Out-of-the-Box Tooling

* **Zero Polling CPU Bloat:** The primary mechanism is purely push-based via native application hooks.
* **Extremely Hackable:** Using standard Unix tools (`fzf`, `jq`, `flock`, `bash`) means debugging is transparent and modifications take seconds without compiling.
* **Token Isolation:** No system prompt padding is required to instruct the LLM on how to report its status; orchestration is fully abstracted to the infrastructure layer.
* **State Deduplication:** The `agent-q push` script parses existing `.jsonl` records by `session_target` to overwrite old entries, preventing automated retry loops from spamming the queue.

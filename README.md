# Agent Orchestrate

A simple, purely shell-based Human-in-the-Loop orchestration queue for your background AI agents using `bash`, `jq`, `fzf`, and `tmux`.

## Installation

1. **Prerequisites**: Ensure you have `jq`, `fzf`, and `tmux` installed on your system.
   ```bash
   brew install jq fzf tmux
   ```

2. **Setup the script**:
   Symlink or place the `agent-q` script somewhere in your `$PATH`.
   ```bash
   ln -s /Users/mloning/Documents/Software/agent-orchestrate/agent-q /usr/local/bin/agent-q
   ```

3. **Configure tmux keybindings**:
   Add the following line to your `~/.tmux.conf` to easily pull up the UI popup with `prefix + i`:
   ```tmux
   bind i run-shell "agent-q popup"
   ```
   After editing, reload your config: `tmux source-file ~/.tmux.conf`

## Usage

### 1. Launch the Watchdog
The watchdog detects crashed or stalled agents in the background. Run it in a background pane or as a service:
```bash
agent-q daemon
```

### 2. Configure Your Agents to Push State
Set your agent hooks to call `agent-q push` when they require permission or stop.

*Example wrapper or hook:*
```bash
agent-q push "claude:1" "claude" "WAITING_APPROVAL" "Proceed with changes?"
```

#### Google Antigravity (`agy`) Integration
This repository comes with pre-built hooks for `agy` to automatically pause and wait for your approval in the queue!
To enable this integration, simply symlink the provided `hooks.json` into your `agy` global config directory:
```bash
ln -s /Users/mloning/Documents/Software/agent-orchestrate/integrations/agy/hooks.json ~/.gemini/config/hooks.json
```

### 3. Triage the Queue
When an agent needs your attention, hit `prefix + i` anywhere in `tmux`.

In the `fzf` popup:
- **`Enter`**: Switch to the agent's tmux window immediately.
- **`y`**: Send 'y' and Enter to the agent's window.
- **`n`**: Send 'n' and Enter to the agent's window.
- **`d`**: Dismiss the alert (deletes it from the queue).
- **`Esc`**: Close the popup and return to work.

## Architecture
See `plan.md` for full architectural details.

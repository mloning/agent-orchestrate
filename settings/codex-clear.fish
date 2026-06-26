# agentq — clear a Codex pane from the dashboard when the agent exits.
#
# Codex has no SessionEnd hook (unlike Claude Code), so there is no hook that can
# run `agentq clear` on exit. Instead, wrap the `codex` command to clear the
# pane's dashboard entry once an interactive session ends.
#
# Install: copy/symlink to ~/.config/fish/conf.d/agentq-codex.fish (fish loads it
# automatically), or let scripts/install-codex-hooks.sh do it.
#
# The `status is-interactive` guard means non-interactive `codex exec` / `codex
# review` (e.g. run from scripts or other agents) never clears a pane — only the
# interactive TUI session you launched does.

function codex --wraps codex --description 'codex + agentq clear on exit'
    command codex $argv
    set -l _agentq_exit $status
    if status is-interactive
        agentq clear 2>/dev/null
    end
    return $_agentq_exit
end

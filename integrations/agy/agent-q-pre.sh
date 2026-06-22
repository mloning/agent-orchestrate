#!/usr/bin/env bash
input=$(cat)
tool_name=$(echo "$input" | jq -r '.toolCall.name')
target="${TMUX_PANE:-agy_${RANDOM}}"

# Try to find agent-q in PATH, otherwise use the repo default
if command -v agent-q >/dev/null 2>&1; then
    AGENT_Q="agent-q"
else
    # Fallback to absolute repo path assuming this script is in integrations/agy
    DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    AGENT_Q="$DIR/../../agent-q"
fi

"$AGENT_Q" push "$target" "agy" "WAITING_APPROVAL" "Pending: $tool_name"
echo '{"decision": "ask"}'

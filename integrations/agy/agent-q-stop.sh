#!/usr/bin/env bash
input=$(cat)
target="${TMUX_PANE:-agy_${RANDOM}}"

if command -v agent-q >/dev/null 2>&1; then
    AGENT_Q="agent-q"
else
    DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    AGENT_Q="$DIR/../../agent-q"
fi

"$AGENT_Q" rm "$target"
echo '{"decision": "stop"}'

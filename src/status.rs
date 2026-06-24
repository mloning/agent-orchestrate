use std::env;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;

use crate::tmux;

/// Tag the current pane with the given status and optional message.
///
/// Reads `$TMUX_PANE` to identify the pane. If unset (i.e. not running
/// inside tmux), this is a silent no-op so hooks can be called safely from
/// any context. Must complete in < 10 ms.
pub fn run(status: &str, message: &str) -> Result<()> {
    let pane = match env::var("TMUX_PANE") {
        Ok(p) if !p.is_empty() => p,
        _ => return Ok(()), // not inside tmux — silent no-op
    };

    let agent_type = env::var("AGENTQ_TYPE").unwrap_or_else(|_| "unknown".to_string());

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .to_string();

    tmux::set_status(&pane, status, message, &agent_type, &ts)
}

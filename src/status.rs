use std::env;

use anyhow::Result;

use crate::model;
use crate::tmux;

/// Tag the current pane with the given status, message, and agent type.
///
/// Reads `$TMUX_PANE` to identify the pane. If unset (i.e. not running
/// inside tmux), this is a silent no-op so hooks can be called safely from
/// any context. Must complete in < 10 ms.
pub fn run(status: &str, message: &str, agent_type: &str) -> Result<()> {
    let pane = match env::var("TMUX_PANE") {
        Ok(p) if !p.is_empty() => p,
        _ => return Ok(()), // not inside tmux — silent no-op
    };

    let ts = model::now_unix_secs().to_string();

    tmux::set_status(&pane, status, message, agent_type, &ts)
}

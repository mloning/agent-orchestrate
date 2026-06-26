use std::env;
use std::io::{IsTerminal, Read};

use anyhow::Result;

use crate::model;
use crate::tmux;

/// Tag the current pane with the given status, message, and agent type.
///
/// Reads `$TMUX_PANE` to identify the pane. If unset (i.e. not running
/// inside tmux), this is a silent no-op so hooks can be called safely from
/// any context. Must complete in < 10 ms.
///
/// When no explicit message is given, fills it from the hook's stdin JSON
/// (the agent's working directory → project name) so the dashboard's MESSAGE
/// column shows which project each agent is in.
pub fn run(status: &str, message: &str, agent_type: &str) -> Result<()> {
    let pane = match env::var("TMUX_PANE") {
        Ok(p) if !p.is_empty() => p,
        _ => return Ok(()), // not inside tmux — silent no-op
    };

    let ts = model::now_unix_secs().to_string();
    let msg = if message.is_empty() {
        message_from_hook_stdin().unwrap_or_default()
    } else {
        message.to_string()
    };

    tmux::set_status(&pane, status, &msg, agent_type, &ts)
}

/// Hooks deliver their context as JSON on stdin. Read it (only when stdin is a
/// pipe, so manual runs don't block) and derive a short message from `cwd` —
/// the project directory name. Best-effort: any failure yields `None`.
fn message_from_hook_stdin() -> Option<String> {
    if std::io::stdin().is_terminal() {
        return None;
    }
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf).ok()?;
    let v: serde_json::Value = serde_json::from_str(buf.trim()).ok()?;
    let cwd = v.get("cwd")?.as_str()?;
    let name = std::path::Path::new(cwd).file_name()?.to_str()?;
    Some(name.to_string())
}

/// Remove the current pane from the dashboard by unsetting its agent options.
/// Hook target for session-end (e.g. Claude `SessionEnd`); no-op outside tmux.
pub fn clear() -> Result<()> {
    let pane = match env::var("TMUX_PANE") {
        Ok(p) if !p.is_empty() => p,
        _ => return Ok(()),
    };
    tmux::clear_status(&pane)
}

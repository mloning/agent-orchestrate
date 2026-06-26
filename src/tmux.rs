use std::process::Command;

use anyhow::{Context, Result};

use crate::model::{self, Agent};

// ---------------------------------------------------------------------------
// Private helpers — the ONLY place that shells out to tmux
// ---------------------------------------------------------------------------

fn run_tmux(args: &[&str]) -> Result<()> {
    let st = Command::new("tmux")
        .args(args)
        .status()
        .context("failed to run tmux")?;
    if !st.success() {
        anyhow::bail!("tmux {} exited with {}", args.join(" "), st);
    }
    Ok(())
}

fn run_tmux_output(args: &[&str]) -> Result<String> {
    let out = Command::new("tmux")
        .args(args)
        .output()
        .context("failed to run tmux")?;
    if !out.status.success() {
        anyhow::bail!(
            "tmux {} exited with {}",
            args.join(" "),
            out.status
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Set all agent metadata on a pane in ONE tmux invocation using `;`-separated
/// commands to keep latency under 10 ms.
pub fn set_status(
    pane: &str,
    status: &str,
    msg: &str,
    agent_type: &str,
    ts: &str,
) -> Result<()> {
    // The message is stored on one tab-delimited `list-panes` field, so strip
    // any tabs/newlines that would otherwise split a row or shift columns.
    let msg = sanitize(msg);
    run_tmux(&[
        "set-option", "-t", pane, "-p", "@agent_status", status, ";",
        "set-option", "-t", pane, "-p", "@agent_msg", &msg, ";",
        "set-option", "-t", pane, "-p", "@agent_updated", ts, ";",
        "set-option", "-t", pane, "-p", "@agent_type", agent_type,
    ])
}

/// Collapse tabs/newlines/carriage-returns to spaces so a value is safe to
/// store in a tab-delimited, line-based `list-panes` field.
fn sanitize(s: &str) -> String {
    s.replace(['\t', '\n', '\r'], " ")
}

/// Unset all agent options on a pane (`-u`), so `list_panes` no longer returns
/// it — i.e. remove the agent from the dashboard. Used by `agentq clear` (the
/// SessionEnd hook) and the TUI's manual clear.
pub fn clear_status(pane: &str) -> Result<()> {
    run_tmux(&[
        "set-option", "-u", "-p", "-t", pane, "@agent_status", ";",
        "set-option", "-u", "-p", "-t", pane, "@agent_msg", ";",
        "set-option", "-u", "-p", "-t", pane, "@agent_updated", ";",
        "set-option", "-u", "-p", "-t", pane, "@agent_type",
    ])
}

/// List all panes across all sessions and return parsed, sorted `Agent`s.
pub fn list_panes() -> Vec<Agent> {
    // FR2: location is reported as `session:window`. The pane id (first field)
    // remains the unique key used for warp/send.
    let format = "#{pane_id}\t#{@agent_status}\t#{session_name}:#{window_index}\t#{@agent_type}\t#{@agent_updated}\t#{@agent_msg}";
    let output = match run_tmux_output(&["list-panes", "-a", "-F", format]) {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };
    let mut agents: Vec<Agent> = output
        .lines()
        .filter_map(model::parse_pane_line)
        .collect();
    agents.sort();
    agents
}

/// Switch to the target pane (across sessions/windows). FR8: land on the
/// EXACT pane, not just its window. A `#{pane_id}` resolves to its session for
/// `switch-client` and its window for `select-window`.
pub fn warp(pane_id: &str) -> Result<()> {
    run_tmux(&[
        "switch-client", "-t", pane_id, ";",
        "select-window", "-t", pane_id, ";",
        "select-pane", "-t", pane_id,
    ])
}

/// Send keys to a pane (with Enter).
pub fn send_keys(pane_id: &str, keys: &str) -> Result<()> {
    run_tmux(&["send-keys", "-t", pane_id, keys, "Enter"])
}

/// Send free text to a pane, using `--` for flag safety.
pub fn send_line(pane_id: &str, text: &str) -> Result<()> {
    run_tmux(&["send-keys", "-t", pane_id, "--", text, "Enter"])
}

/// Capture the last N lines from a pane as plain text (no escape sequences).
pub fn capture_pane(pane_id: &str, lines: u32) -> Result<String> {
    let start = format!("-{}", lines);
    run_tmux_output(&[
        "capture-pane", "-t", pane_id, "-p", "-S", &start,
    ])
}

/// Capture the last N lines from a pane WITH ANSI escape sequences (`-e`), so a
/// colored preview can be rendered. Kept separate from `capture_pane` so the
/// watcher's signature matching stays on clean, escape-free text.
pub fn capture_pane_ansi(pane_id: &str, lines: u32) -> Result<String> {
    let start = format!("-{}", lines);
    run_tmux_output(&[
        "capture-pane", "-t", pane_id, "-e", "-p", "-S", &start,
    ])
}

/// Check if a tmux session exists.
pub fn has_session(name: &str) -> bool {
    run_tmux(&["has-session", "-t", name]).is_ok()
}

/// Create a new tmux session (detached).
pub fn new_session(name: &str, window_name: &str, command: &str) -> Result<()> {
    run_tmux(&[
        "new-session", "-d", "-s", name, "-n", window_name, command,
    ])
}

/// Return the name of the current session.
pub fn current_session() -> Result<String> {
    run_tmux_output(&["display-message", "-p", "#{session_name}"])
}

/// Return the pane ID of the current pane.
pub fn current_pane() -> Result<String> {
    run_tmux_output(&["display-message", "-p", "#{pane_id}"])
}

/// Set a global tmux option.
pub fn set_global_option(key: &str, value: &str) -> Result<()> {
    run_tmux(&["set-option", "-g", key, value])
}

/// Get a global tmux option.
pub fn get_global_option(key: &str) -> Result<String> {
    run_tmux_output(&["show-option", "-gv", key])
}

/// Switch the current client to a different session.
pub fn switch_client(target: &str) -> Result<()> {
    run_tmux(&["switch-client", "-t", target])
}

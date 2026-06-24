use anyhow::Result;

use crate::tmux;

/// Toggle the agentq dashboard session.
///
/// - If already in the "agentq" session → read `@agentq_origin` and switch
///   back to the origin pane.
/// - Otherwise → save the current pane as `@agentq_origin`, ensure the
///   agentq session exists (create it with `agentq tui` if needed), and
///   switch to it.
pub fn run() -> Result<()> {
    let session = tmux::current_session()?;

    if session == "agentq" {
        // Return to origin
        let origin = tmux::get_global_option("@agentq_origin")?;
        if !origin.is_empty() {
            tmux::switch_client(&origin)?;
        }
    } else {
        // Save origin and jump to dashboard
        let pane = tmux::current_pane()?;
        tmux::set_global_option("@agentq_origin", &pane)?;

        if !tmux::has_session("agentq") {
            tmux::new_session("agentq", "dashboard", "agentq tui")?;
        }

        tmux::switch_client("agentq")?;
    }

    Ok(())
}

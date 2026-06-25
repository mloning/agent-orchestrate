mod model;
mod open;
mod status;
mod tmux;
mod tui;
mod watch;

use clap::{Parser, Subcommand};

/// Agent triage dashboard for tmux
#[derive(Parser)]
#[command(name = "agentq", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Hook target; tags the current pane with a status
    Status {
        /// The status to set (e.g. RUNNING, CRASHED, IDLE)
        status: String,
        /// Optional message to attach
        #[arg(default_value = "")]
        message: String,
        /// Agent type shown in the dashboard (claude | codex | gemini)
        #[arg(long = "type", short = 't', default_value = "unknown")]
        agent_type: String,
    },
    /// Launch the persistent live dashboard
    Tui,
    /// Summon / toggle / return binding
    Open,
    /// Crash / stall detection loop (Phase 2)
    Watch,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Status {
            status,
            message,
            agent_type,
        } => status::run(&status, &message, &agent_type),
        Command::Tui => tui::run(),
        Command::Open => open::run(),
        Command::Watch => watch::run(),
    }
}

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
        Command::Status { status, message } => status::run(&status, &message),
        Command::Tui => tui::run(),
        Command::Open => open::run(),
        Command::Watch => watch::run(),
    }
}

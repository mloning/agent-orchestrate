mod model;
mod open;
mod status;
mod summarize;
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
        /// The status to set (e.g. RUNNING, WAITING, IDLE)
        status: String,
        /// Optional message to attach
        #[arg(default_value = "")]
        message: String,
        /// Agent type shown in the dashboard (claude | codex | gemini)
        #[arg(long = "type", short = 't', default_value = "unknown")]
        agent_type: String,
    },
    /// Remove the current pane from the dashboard (hook target for session end)
    Clear,
    /// Hook target (turn-end); compute the pane's stable topic once, in the
    /// background, via `claude -p`. No-op if a topic is already set.
    Summarize {
        /// Agent type, mirrored from the status hooks (claude | codex | gemini)
        #[arg(long = "type", short = 't', default_value = "unknown")]
        agent_type: String,
    },
    /// Internal: detached worker that does the actual summarization (spawned by
    /// `summarize`; not meant to be invoked directly).
    #[command(hide = true)]
    SummarizeWorker {
        #[arg(long)]
        pane: String,
        #[arg(long = "type", default_value = "unknown")]
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
        Command::Clear => status::clear(),
        Command::Summarize { agent_type } => summarize::run(&agent_type),
        Command::SummarizeWorker { pane, agent_type } => summarize::run_worker(&pane, &agent_type),
        Command::Tui => tui::run(),
        Command::Open => open::run(),
        Command::Watch => watch::run(),
    }
}

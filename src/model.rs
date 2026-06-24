use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

use ratatui::style::Color;

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Crashed,
    Stalled,
    WaitingApproval,
    WaitingInput,
    Running,
    Idle,
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Status::Crashed => "CRASHED",
            Status::Stalled => "STALLED",
            Status::WaitingApproval => "WAITING_APPROVAL",
            Status::WaitingInput => "WAITING_INPUT",
            Status::Running => "RUNNING",
            Status::Idle => "IDLE",
        };
        write!(f, "{}", label)
    }
}

impl FromStr for Status {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_uppercase().as_str() {
            "CRASHED" => Ok(Status::Crashed),
            "STALLED" => Ok(Status::Stalled),
            "WAITING_APPROVAL" => Ok(Status::WaitingApproval),
            "WAITING_INPUT" => Ok(Status::WaitingInput),
            "RUNNING" => Ok(Status::Running),
            "IDLE" => Ok(Status::Idle),
            other => anyhow::bail!("unknown status: {other}"),
        }
    }
}

impl Status {
    /// Priority tier — lower means more urgent.
    pub fn tier(&self) -> u8 {
        match self {
            Status::Crashed | Status::Stalled => 0,
            Status::WaitingApproval => 1,
            Status::WaitingInput => 2,
            Status::Running => 3,
            Status::Idle => 4,
        }
    }

    /// Returns true for statuses that need human attention.
    pub fn is_attention(&self) -> bool {
        matches!(
            self,
            Status::Crashed | Status::Stalled | Status::WaitingApproval | Status::WaitingInput
        )
    }

    /// TUI color for this status.
    pub fn color(&self) -> Color {
        match self {
            Status::Crashed | Status::Stalled => Color::Red,
            Status::WaitingApproval => Color::Yellow,
            Status::WaitingInput => Color::Magenta,
            Status::Running => Color::Green,
            Status::Idle => Color::DarkGray,
        }
    }
}

// ---------------------------------------------------------------------------
// Agent
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Agent {
    pub pane_id: String,
    pub status: Status,
    pub agent_type: String,
    pub location: String,
    pub updated: u64,
    pub message: String,
}

impl Eq for Agent {}

impl PartialEq for Agent {
    fn eq(&self, other: &Self) -> bool {
        self.pane_id == other.pane_id
    }
}

impl Ord for Agent {
    fn cmp(&self, other: &Self) -> Ordering {
        self.status
            .tier()
            .cmp(&other.status.tier())
            .then_with(|| self.updated.cmp(&other.updated))
    }
}

impl PartialOrd for Agent {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Parse a tab-separated pane line:
/// `pane_id\tstatus\tlocation\tagent_type\tupdated\tmessage`
///
/// Returns `None` if the status field is empty or unparseable.
pub fn parse_pane_line(line: &str) -> Option<Agent> {
    let fields: Vec<&str> = line.splitn(6, '\t').collect();
    if fields.len() < 5 {
        return None;
    }

    let raw_status = fields[1];
    if raw_status.is_empty() {
        return None;
    }

    let status: Status = raw_status.parse().ok()?;
    let updated: u64 = fields[4].parse().unwrap_or(0);
    let message = if fields.len() > 5 { fields[5] } else { "" };

    Some(Agent {
        pane_id: fields[0].to_string(),
        status,
        agent_type: fields[3].to_string(),
        location: fields[2].to_string(),
        updated,
        message: message.to_string(),
    })
}

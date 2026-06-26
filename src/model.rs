use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::style::Color;

// ---------------------------------------------------------------------------
// Time helpers
// ---------------------------------------------------------------------------

/// Current unix time in seconds. Used for `@agent_updated` and age display.
pub fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Render an elapsed duration at minute granularity (no ticking seconds clock):
/// `<1 min`, `1 min`, `2 min`, then `1h`, `2h`, `1d`, `2d`.
pub fn humanize_age(secs: u64) -> String {
    if secs < 60 {
        "<1 min".to_string()
    } else if secs < 3600 {
        format!("{} min", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

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

    /// Human-friendly label for the dashboard. Distinct from `Display`, which
    /// stays the canonical SCREAMING form used for storage and parsing.
    pub fn label(&self) -> &'static str {
        match self {
            Status::Crashed => "Crashed",
            Status::Stalled => "Stalled",
            Status::WaitingApproval => "Waiting for approval",
            Status::WaitingInput => "Waiting for input",
            Status::Running => "Running",
            Status::Idle => "Idle",
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

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(pane: &str, status: Status, updated: u64) -> Agent {
        Agent {
            pane_id: pane.into(),
            status,
            agent_type: "claude".into(),
            location: "work:0".into(),
            updated,
            message: String::new(),
        }
    }

    #[test]
    fn parses_full_line() {
        let line = "%3\tWAITING_APPROVAL\twork:1\tcodex\t1700\trun tests?";
        let a = parse_pane_line(line).expect("should parse");
        assert_eq!(a.pane_id, "%3");
        assert_eq!(a.status, Status::WaitingApproval);
        assert_eq!(a.location, "work:1");
        assert_eq!(a.agent_type, "codex");
        assert_eq!(a.updated, 1700);
        assert_eq!(a.message, "run tests?");
    }

    #[test]
    fn message_may_contain_tabs() {
        // splitn(6) keeps everything after the 5th tab in the message field.
        let a = parse_pane_line("%1\tRUNNING\tw:0\tclaude\t10\ta\tb").unwrap();
        assert_eq!(a.message, "a\tb");
    }

    #[test]
    fn drops_unregistered_and_unknown() {
        // empty @agent_status (pane never fired a hook)
        assert!(parse_pane_line("%1\t\tw:0\tclaude\t10\t").is_none());
        // unparseable status
        assert!(parse_pane_line("%1\tBOGUS\tw:0\tclaude\t10\t").is_none());
    }

    #[test]
    fn fr3_tier_order_then_oldest_first() {
        // Deliberately out of order; sorting must yield the FR3 tiers.
        let mut v = [
            agent("%idle", Status::Idle, 1),
            agent("%run", Status::Running, 1),
            agent("%wi", Status::WaitingInput, 1),
            agent("%wa", Status::WaitingApproval, 1),
            agent("%stall", Status::Stalled, 1),
            agent("%crash", Status::Crashed, 1),
        ];
        v.sort();
        let order: Vec<&str> = v.iter().map(|a| a.pane_id.as_str()).collect();
        // CRASHED/STALLED share tier 0 (stable order between them is fine).
        assert_eq!(order[2], "%wa");
        assert_eq!(order[3], "%wi");
        assert_eq!(order[4], "%run");
        assert_eq!(order[5], "%idle");
        assert!(v[0].status.tier() == 0 && v[1].status.tier() == 0);
    }

    #[test]
    fn within_tier_oldest_waiting_first() {
        let mut v = [
            agent("%new", Status::WaitingApproval, 200),
            agent("%old", Status::WaitingApproval, 100),
        ];
        v.sort();
        assert_eq!(v[0].pane_id, "%old"); // smaller @agent_updated = older = higher
    }

    #[test]
    fn humanize_age_units() {
        assert_eq!(humanize_age(5), "<1 min");
        assert_eq!(humanize_age(59), "<1 min");
        assert_eq!(humanize_age(60), "1 min");
        assert_eq!(humanize_age(125), "2 min");
        assert_eq!(humanize_age(7200), "2h");
        assert_eq!(humanize_age(172_800), "2d");
    }
}

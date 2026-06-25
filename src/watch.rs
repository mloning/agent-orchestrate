use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::thread;
use std::time::Duration;

use anyhow::Result;

use crate::model::{self, Agent, Status};
use crate::tmux;

// --- Tunables (OQ-1) -------------------------------------------------------

/// How often the watcher scans every registered pane.
const INTERVAL: Duration = Duration::from_secs(25);
/// RUNNING with unchanged output for this long → STALLED.
const STALL_SECS: u64 = 600; // 10 minutes
/// Lines of pane tail to scrape for crash/plan signatures.
const CAPTURE_LINES: u32 = 40;

// --- Signatures (heuristic; tune to keep false positives rare, NFR7) -------

/// If ANY of these appear in the tail, the agent's TUI is still alive, so the
/// pane is NOT crashed. `❯` is deliberately NOT here: it must never be read as a
/// crash signal (NFR6 — Codex/starship render it healthy), but treating it as
/// proof-of-alive would let a crashed Codex pane sitting at a fish shell stay
/// undetected as long as `❯` lingers in its scrollback. The `❯` exclusion in
/// `looks_like_bare_shell` keeps a healthy `❯` prompt from being flagged.
const AGENT_CHROME: &[&str] = &[
    "esc to interrupt",
    "for shortcuts",
    "Bypassing Permissions",
    "auto-accept edits",
    "/help",
];

/// Strong crash/exit signatures: language stack traces and shell errors that
/// only appear once an agent has died to a shell.
const CRASH_SIGNATURES: &[&str] = &[
    "Traceback (most recent call last):",
    "node:internal/",
    "npm ERR!",
    "command not found",
    "Segmentation fault",
    "core dumped",
    "panic:",
    "fatal runtime error",
    "zsh:",
    "bash:",
    "Killed",
    "Aborted (core dumped)",
];

/// Claude plan-approval prompt — emits no hook (issue #19283; OQ-3 default
/// yes), so the watcher recovers it by matching the on-screen prompt text.
/// Claude-only.
const PLAN_PROMPT_MARKERS: &[&str] = &[
    "Would you like to proceed?",
    "Accept this plan",
    "Ready to code?",
    "keep planning",
];

// ---------------------------------------------------------------------------

/// Per-pane observation used for output-change stall detection.
struct Obs {
    /// Hash of the last captured tail.
    hash: u64,
    /// Unix seconds when the output last changed.
    since: u64,
}

/// Crash / stall detection loop (FR9). Detection only — never registration
/// (NG3): it acts solely on panes that already carry `@agent_status`.
pub fn run() -> Result<()> {
    let mut obs: HashMap<String, Obs> = HashMap::new();
    loop {
        scan_once(&mut obs);
        thread::sleep(INTERVAL);
    }
}

fn scan_once(obs: &mut HashMap<String, Obs>) {
    let now = model::now_unix_secs();
    let agents = tmux::list_panes();

    // Drop observations for panes that no longer exist (NFR5: self-cleaning).
    let live: HashSet<&str> = agents.iter().map(|a| a.pane_id.as_str()).collect();
    obs.retain(|k, _| live.contains(k.as_str()));

    for agent in &agents {
        // Leave already-crashed panes alone — no churn.
        if agent.status == Status::Crashed {
            continue;
        }

        let tail = tmux::capture_pane(&agent.pane_id, CAPTURE_LINES).unwrap_or_default();

        if looks_crashed(&tail) {
            let _ = set(agent, Status::Crashed, "dropped to shell", now);
            obs.remove(&agent.pane_id);
            continue;
        }

        // Recover the Claude plan-approval gap (no hook fires).
        if agent.agent_type == "claude"
            && agent.status != Status::WaitingApproval
            && is_plan_prompt(&tail)
        {
            let _ = set(agent, Status::WaitingApproval, "plan approval", now);
            continue;
        }

        // Stall: RUNNING with no output change past the threshold. Keyed on the
        // captured tail rather than `@agent_updated`, so a healthy agent that
        // is actively printing is never flagged.
        if agent.status == Status::Running {
            let h = hash_str(&tail);
            let entry = obs.entry(agent.pane_id.clone()).or_insert(Obs { hash: h, since: now });
            if entry.hash != h {
                entry.hash = h;
                entry.since = now;
            } else if now.saturating_sub(entry.since) >= STALL_SECS {
                let _ = set(agent, Status::Stalled, "no progress", now);
            }
        } else {
            obs.remove(&agent.pane_id);
        }
    }
}

/// A registered pane is treated as crashed when its TUI chrome is gone AND
/// either a crash signature is present or it has fallen back to a bare shell
/// prompt.
fn looks_crashed(tail: &str) -> bool {
    if AGENT_CHROME.iter().any(|m| tail.contains(m)) {
        return false;
    }
    if CRASH_SIGNATURES.iter().any(|m| tail.contains(m)) {
        return true;
    }
    looks_like_bare_shell(tail)
}

/// Heuristic bare-shell detection on the last non-empty line. Fish-aware: the
/// default fish prompt ends with the cwd and `>`. `❯` is excluded — it is a
/// healthy Codex/starship prompt, not a crash (NFR6).
fn looks_like_bare_shell(tail: &str) -> bool {
    let last = tail
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim_end();
    if last.contains('❯') {
        return false;
    }
    last.ends_with('$') || last.ends_with('%') || last.ends_with('#') || last.ends_with('>')
}

fn is_plan_prompt(tail: &str) -> bool {
    PLAN_PROMPT_MARKERS.iter().any(|m| tail.contains(m))
}

fn set(agent: &Agent, status: Status, msg: &str, now: u64) -> Result<()> {
    tmux::set_status(
        &agent.pane_id,
        &status.to_string(),
        msg,
        &agent.agent_type,
        &now.to_string(),
    )
}

fn hash_str(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_fish_shell_is_a_crash() {
        // Agent died to a bare fish prompt (AC8): no chrome, prompt ends with `>`.
        let tail = "Some earlier output\nmloning@host ~/Dev/projects/agent-orchestrate>";
        assert!(looks_crashed(tail));
    }

    #[test]
    fn healthy_codex_prompt_is_not_a_crash() {
        // NFR6 / AC8: a healthy Codex `❯` prompt must NEVER be flagged.
        let tail = "working on it\n❯ ";
        assert!(!looks_crashed(tail));
        assert!(!looks_like_bare_shell(tail));
    }

    #[test]
    fn crashed_codex_with_arrow_in_scrollback_is_detected() {
        // Codex died to a bare fish shell; its `❯` UI lingers in scrollback but
        // the last line is the fish prompt. Must still be flagged crashed — `❯`
        // is not treated as proof-of-alive (regression test for that fix).
        let tail = "❯ run the build\nbuilding...\nmloning@host ~/proj>";
        assert!(looks_crashed(tail));
    }

    #[test]
    fn stack_trace_without_chrome_is_a_crash() {
        let tail = "Traceback (most recent call last):\n  File \"x.py\"\nValueError: boom";
        assert!(looks_crashed(tail));
    }

    #[test]
    fn chrome_present_overrides_crash_signatures() {
        // If the agent UI is still drawing, it is alive even if scrollback shows
        // an error string.
        let tail = "npm ERR! something\n│ > prompt          esc to interrupt │";
        assert!(!looks_crashed(tail));
    }

    #[test]
    fn live_agent_input_box_is_not_a_crash() {
        let tail = "│ Try \"fix the bug\"                        │\n│ ? for shortcuts                          │";
        assert!(!looks_crashed(tail));
    }

    #[test]
    fn detects_plan_prompt() {
        assert!(is_plan_prompt("Here is the plan.\n\nWould you like to proceed?"));
        assert!(!is_plan_prompt("just some normal output"));
    }
}

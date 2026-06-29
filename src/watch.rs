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
/// Bottom-most lines treated as the pane's LIVE region. The plan-prompt box,
/// the working-spinner footer, and a crash's stack trace + shell prompt are all
/// pinned to the bottom of the screen; once superseded they scroll up out of
/// this window (but stay in the wider `CAPTURE_LINES` scrollback). Scoping
/// detection to the live region is what keeps a marker left in scrollback from
/// re-triggering — the bug where a resumed agent snapped back to
/// WAITING_APPROVAL, and the bug where a tool-call's traceback in scrollback
/// was read as the agent itself crashing.
const LIVE_REGION_LINES: usize = 8;

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
/// only appear once an agent has died to a shell. Matched ONLY within the LIVE
/// region (see `looks_crashed`): agents routinely run tools that print a stack
/// trace as ordinary output (`python -c …`, a failing test), then read it and
/// keep working — that trace belongs in scrollback, not the bottom of the pane.
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

/// Sentinel message stamped on a watcher-driven plan wait. It doubles as the
/// discriminator for the plan auto-resume below: the watcher resumes a plan wait
/// purely on the prompt disappearing, which is only safe for the waits IT
/// created (a real `PermissionRequest` wait carries the project name and must
/// stay until the human answers it).
const PLAN_APPROVAL_MSG: &str = "plan approval";

/// "Actively working" footer markers — the interrupt hint agents render only
/// while a turn is running, never while a prompt awaits input. Matched
/// case-insensitively so both Claude's `esc to interrupt` and Codex's
/// `Esc to interrupt` count. This is the signal used to resume a stale
/// approval wait answered in the agent's own pane (tool-permission approvals
/// fire no "granted" hook). Tune here if an agent's wording differs.
const WORKING_MARKERS: &[&str] = &["esc to interrupt"];

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
    log(&format!(
        "started — scanning every {}s, stall after {}s",
        INTERVAL.as_secs(),
        STALL_SECS
    ));
    let mut obs: HashMap<String, Obs> = HashMap::new();
    loop {
        scan_once(&mut obs);
        thread::sleep(INTERVAL);
    }
}

/// Emit a line to stderr (unbuffered, so it shows promptly under `tail -f` and
/// in the launchd log). Prefixed for easy grepping.
fn log(msg: &str) {
    eprintln!("[agentq watch] {msg}");
}

fn scan_once(obs: &mut HashMap<String, Obs>) {
    let now = model::now_unix_secs();
    let agents = tmux::list_panes();
    log(&format!("scan: {} registered pane(s)", agents.len()));

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
            log(&format!(
                "CRASHED {} ({}, {}) — dropped to shell",
                agent.pane_id, agent.agent_type, agent.location
            ));
            let _ = set(agent, Status::Crashed, "dropped to shell", now);
            obs.remove(&agent.pane_id);
            continue;
        }

        // Raise the Claude plan-approval gap: it fires no hook (issue #19283;
        // OQ-3 default yes), so the watcher detects it from the on-screen prompt.
        // Detection is scoped to the LIVE region (`is_active_plan_prompt`) so an
        // answered prompt lingering in scrollback never re-triggers — the bug
        // where a resumed agent snapped back to WAITING_APPROVAL.
        if agent.agent_type == "claude"
            && agent.status != Status::WaitingApproval
            && is_active_plan_prompt(&tail)
        {
            log(&format!(
                "WAITING_APPROVAL {} ({}, {}) — plan-approval prompt",
                agent.pane_id, agent.agent_type, agent.location
            ));
            let _ = set(agent, Status::WaitingApproval, PLAN_APPROVAL_MSG, now);
            continue;
        }

        // Resume a stale approval wait. Neither a plan approval nor a tool
        // permission fires a "granted" hook, so a prompt answered in the agent's
        // own pane would otherwise sit at WAITING_APPROVAL until the turn ends.
        // Two safe signals, both confined to the LIVE region so a marker left in
        // scrollback can't fire a false resume:
        //   - the agent is visibly working again (interrupt-hint footer), or
        //   - a watcher-raised plan wait (sentinel msg) whose prompt is now gone.
        // Conservatively gated (NFR7, and the #1 goal of never hiding a real
        // prompt): the interrupt hint shows only while working — never while a
        // prompt is up — so the failure direction is a missed resume, not a
        // missed prompt. A real PermissionRequest wait still showing its prompt
        // matches neither signal and stays put.
        if agent.status == Status::WaitingApproval {
            let working = looks_working(&tail);
            let answered_plan = agent.message == PLAN_APPROVAL_MSG
                && !is_active_plan_prompt(&tail);
            if working || answered_plan {
                log(&format!(
                    "RUNNING {} ({}, {}) — approval answered ({})",
                    agent.pane_id,
                    agent.agent_type,
                    agent.location,
                    if working { "agent working" } else { "plan prompt gone" }
                ));
                let _ = set(agent, Status::Running, "resumed", now);
                continue;
            }
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
                log(&format!(
                    "STALLED {} ({}, {}) — no output for {}s",
                    agent.pane_id,
                    agent.agent_type,
                    agent.location,
                    now.saturating_sub(entry.since)
                ));
                let _ = set(agent, Status::Stalled, "no progress", now);
            }
        } else {
            obs.remove(&agent.pane_id);
        }
    }
}

/// A registered pane is treated as crashed when its TUI chrome is gone AND
/// either a crash signature shows in the LIVE region or it has fallen back to a
/// bare shell prompt.
///
/// Crash signatures are matched against the live region, NOT the full capture.
/// Agents constantly run tools — `python -c …`, a test suite — that print a
/// stack trace as ordinary tool output; the agent reads it and keeps working,
/// so the trace scrolls up into scrollback with fresh agent output below it.
/// Only a trace still pinned to the bottom of the pane, with no agent output or
/// chrome beneath it, means the agent itself died. Matching the whole capture
/// flagged those healthy panes as crashed (the reported false positive). The
/// chrome veto stays wide (full capture): chrome anywhere is strong proof the
/// TUI is alive, so we err toward NOT flagging a live agent.
fn looks_crashed(tail: &str) -> bool {
    if AGENT_CHROME.iter().any(|m| tail.contains(m)) {
        return false;
    }
    let live = live_region(tail);
    if CRASH_SIGNATURES.iter().any(|m| live.contains(m)) {
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

/// True only when a plan prompt is the LIVE prompt — its markers appear in the
/// bottom `LIVE_REGION_LINES` of the capture. An answered prompt has fresh agent
/// output below it, so its markers fall outside this window and read as inactive.
fn is_active_plan_prompt(tail: &str) -> bool {
    is_plan_prompt(&live_region(tail))
}

/// True when the agent's live footer shows it is actively processing a turn (the
/// interrupt hint). Scoped to the LIVE region and matched case-insensitively;
/// used to resume an approval wait answered in the agent's own pane.
fn looks_working(tail: &str) -> bool {
    let region = live_region(tail).to_ascii_lowercase();
    WORKING_MARKERS.iter().any(|m| region.contains(m))
}

/// The bottom `LIVE_REGION_LINES` of a capture, joined back into one string.
fn live_region(tail: &str) -> String {
    let lines: Vec<&str> = tail.lines().collect();
    let start = lines.len().saturating_sub(LIVE_REGION_LINES);
    lines[start..].join("\n")
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
    fn tool_traceback_in_scrollback_is_not_a_crash() {
        // Regression (reported bug): the agent ran `python -c …` through a Bash
        // tool; the command exited 1 and printed a Python traceback as TOOL
        // OUTPUT, then the agent read it and kept working. The trace sits in
        // scrollback with agent analysis + a recap rendered below it — the agent
        // never died, so the pane must NOT be flagged crashed.
        let mut lines = vec![
            "  ⎿  Error: Exit code 1",
            "     Traceback (most recent call last):",
            "       File \"pandas/core/apply.py\", line 314, in transform",
            "     KeyError: 'model_name'",
        ];
        // Agent output below the trace pushes it up out of the live region.
        for _ in 0..LIVE_REGION_LINES {
            lines.push("⏺ Reproduced exactly — same KeyError chain as the PR.");
        }
        lines.push("※ recap: confirmed the repro; next decide whether to fix.");
        let tail = lines.join("\n");
        assert!(tail.contains("Traceback (most recent call last):")); // in the capture…
        assert!(!looks_crashed(&tail)); // …but not in the live region → not a crash
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

    #[test]
    fn active_plan_prompt_detected_when_box_is_at_bottom() {
        // Unanswered: the interactive box is pinned to the bottom of the pane.
        let tail = "earlier output\n\
                    ╭─ plan ─╮\n\
                    │ Ready to code?\n\
                    │ ❯ 1. Yes\n\
                    │   2. No, keep planning\n\
                    ╰────────╯";
        assert!(is_active_plan_prompt(tail));
    }

    #[test]
    fn answered_plan_prompt_in_scrollback_is_not_active() {
        // The prompt was answered and the agent resumed; the markers have
        // scrolled up out of the live region even though they remain in the
        // wider capture. This is the regression that caused a running agent to
        // snap back to WAITING_APPROVAL.
        let mut lines = vec!["Ready to code?", "❯ 1. Yes", "  2. No, keep planning"];
        for _ in 0..LIVE_REGION_LINES {
            lines.push("⏺ working on the plan…");
        }
        let tail = lines.join("\n");
        assert!(is_plan_prompt(&tail)); // still present in the full capture
        assert!(!is_active_plan_prompt(&tail)); // …but no longer the live prompt
    }

    #[test]
    fn working_footer_signals_active_turn() {
        // Claude and Codex interrupt-hint footers, matched case-insensitively.
        assert!(looks_working("⏺ Editing files…\n  ✻ Cogitating (12s · esc to interrupt)"));
        assert!(looks_working("working\n  Press Esc to interrupt"));
        assert!(!looks_working("idle\n> "));
    }

    #[test]
    fn working_hint_in_scrollback_does_not_signal() {
        // The hint scrolled up out of the live footer (e.g. a prompt is now up);
        // it must not read as working.
        let mut lines = vec!["esc to interrupt"];
        for _ in 0..LIVE_REGION_LINES {
            lines.push("Do you want to proceed?");
        }
        assert!(!looks_working(&lines.join("\n")));
    }
}

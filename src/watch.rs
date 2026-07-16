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
/// WAITING, and the bug where a tool-call's traceback in scrollback
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
///
/// Matched line-anchored (`line_is_crash_signature`), NOT as a bare substring:
/// every real signature LEADS its line — the shell/runtime prints it at column 0
/// (`bash: x: command not found`, `Traceback (most recent call last):`,
/// `panic: …`). Agents, by contrast, discuss these same phrases mid-sentence in
/// their prose and end-of-turn `※ recap:` line (`Fixed the command not found
/// error`); a bare-substring match read that prose as the agent itself crashing
/// (the reported false positive, NFR7). Bare `command not found` / `core dumped`
/// are intentionally absent: they never lead a line, and their real forms are
/// already caught here — `bash:`/`zsh:` lead a shell's not-found error, and
/// `Segmentation fault`/`Aborted (core dumped)` lead the dump line.
const CRASH_SIGNATURES: &[&str] = &[
    "Traceback (most recent call last):",
    "node:internal/",
    "npm ERR!",
    "Segmentation fault",
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

/// Claude tool-permission prompt text. Deliberately distinct from the plan
/// prompt's "Would you like to proceed?" wording (`PLAN_PROMPT_MARKERS`) so the
/// two never cross-match. Normally the `PermissionRequest` hook flips these to
/// `WAITING`, but the watcher scrapes for them too as a fallback for
/// prompt variants that fire NO hook — notably the Bash command-safety
/// confirmation (the "brace expansion" / command-injection warning), which
/// leaves the pane stuck at RUNNING with a live prompt on screen. Claude-only.
const TOOL_PROMPT_MARKERS: &[&str] = &["Do you want to proceed?"];

/// Sentinel message stamped on a watcher-driven tool-permission wait. Like
/// `PLAN_APPROVAL_MSG`, it discriminates the waits the watcher created (safe to
/// auto-resume the instant the prompt disappears) from a real `PermissionRequest`
/// wait (message is the project name; stays until the human answers it).
const TOOL_APPROVAL_MSG: &str = "tool approval";

/// Task-status glyphs Claude leads each item of its persistent task-list panel
/// with (`◼` in progress, `◻` open, plus close variants). Used only to recognize
/// the panel so it can be peeled off the bottom of a capture — see
/// `trim_trailing_task_panel`. Kept lenient so a glyph tweak degrades to a missed
/// strip (old behavior), never a false one.
const TASK_GLYPHS: &[&str] = &["◻", "◼", "◧", "◨", "◫", "☐", "☒", "☑", "✔", "✓", "●", "○"];

/// Substring identifying Claude's "run in background" hint footer, e.g.
/// `(ctrl+b ctrl+b (twice) to run in background)`. Claude pins this hint to the
/// very bottom of the pane while a command (or its permission prompt) is live —
/// sometimes several lines of it. Used only to peel that footer off a capture
/// so a prompt above it lands back in the live region — see
/// `trim_trailing_bg_hints`. Kept lenient so a wording tweak degrades to a
/// missed strip (old behavior), never a false one.
const BG_HINT_MARKER: &str = "run in background";

/// "Actively working" footer markers — the interrupt hint agents render only
/// while a turn is running, never while a prompt awaits input. Matched
/// case-insensitively so both Claude's `esc to interrupt` and Codex's
/// `Esc to interrupt` count. This is the signal used to resume a stale
/// approval wait answered in the agent's own pane (tool-permission approvals
/// fire no "granted" hook). Tune here if an agent's wording differs.
const WORKING_MARKERS: &[&str] = &["esc to interrupt"];

/// Claude turn-error signature. When a turn fails unrecoverably (e.g. the API
/// is unreachable after retries are exhausted) Claude renders an `API Error:`
/// assistant line and ENDS the turn — the process stays alive at the Claude
/// TUI, so no `Stop` hook fires and the pane sits stuck at RUNNING. Matched in
/// the LIVE region while the agent is not working (see `looks_errored`), which
/// excludes the transient "Retrying…" footer that keeps the interrupt hint up.
/// Claude-only wording.
const API_ERROR_MARKERS: &[&str] = &["API Error:"];

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

        // Claude API/turn error: the TUI is alive (so `looks_crashed` sees no
        // shell/stack trace) but the turn died with no `Stop` hook, and the
        // `✻ Cooked for …` footer keeps the tail ticking so the stall check never
        // fires either — the pane would otherwise sit stuck at RUNNING. Detect it
        // directly and treat it as CRASHED (needs attention); a re-prompt fires
        // UserPromptSubmit → RUNNING and recovers it. Claude-only.
        if agent.status == Status::Running
            && agent.agent_type == "claude"
            && looks_errored(&tail)
        {
            log(&format!(
                "CRASHED {} ({}, {}) — api error",
                agent.pane_id, agent.agent_type, agent.location
            ));
            let _ = set(agent, Status::Crashed, "api error", now);
            obs.remove(&agent.pane_id);
            continue;
        }

        // Raise the Claude plan-approval gap: it fires no hook (issue #19283;
        // OQ-3 default yes), so the watcher detects it from the on-screen prompt.
        // Detection is scoped to the LIVE region (`is_active_plan_prompt`) so an
        // answered prompt lingering in scrollback never re-triggers — the bug
        // where a resumed agent snapped back to WAITING.
        if agent.agent_type == "claude"
            && agent.status != Status::Waiting
            && is_active_plan_prompt(&tail)
        {
            log(&format!(
                "WAITING {} ({}, {}) — plan-approval prompt",
                agent.pane_id, agent.agent_type, agent.location
            ));
            let _ = set(agent, Status::Waiting, PLAN_APPROVAL_MSG, now);
            continue;
        }

        // Raise a Claude tool-permission prompt that no hook reported. Normally
        // `PermissionRequest` → WAITING covers these within ~500ms, but
        // some prompt variants fire no hook — notably the Bash command-safety
        // confirmation (the "brace expansion" warning) — leaving the pane stuck
        // at RUNNING with a live prompt on screen (the reported gap). Mirrors the
        // plan branch: scoped to the LIVE region so an answered prompt in
        // scrollback never re-triggers, and it never overwrites an existing
        // WAITING, so a hook-set wait keeps its project-name message.
        // The `!looks_working` guard makes it strictly safe: a real prompt shows
        // no interrupt hint, so this only skips a working pane that happens to
        // echo the phrase in its live output — a missed raise, never a false one.
        if agent.agent_type == "claude"
            && agent.status != Status::Waiting
            && is_active_tool_prompt(&tail)
            && !looks_working(&tail)
        {
            log(&format!(
                "WAITING {} ({}, {}) — tool-permission prompt",
                agent.pane_id, agent.agent_type, agent.location
            ));
            let _ = set(agent, Status::Waiting, TOOL_APPROVAL_MSG, now);
            continue;
        }

        // Resume a stale approval wait. Neither a plan approval nor a tool
        // permission fires a "granted" hook, so a prompt answered in the agent's
        // own pane would otherwise sit at WAITING until the turn ends.
        // Two kinds of safe signal, both confined to the LIVE region so a marker
        // left in scrollback can't fire a false resume:
        //   - the agent is visibly working again (interrupt-hint footer), or
        //   - a watcher-raised wait (plan or tool sentinel msg) whose prompt is
        //     now gone.
        // Conservatively gated (NFR7, and the #1 goal of never hiding a real
        // prompt): the interrupt hint shows only while working — never while a
        // prompt is up — so the failure direction is a missed resume, not a
        // missed prompt. A real PermissionRequest wait still showing its prompt
        // matches neither signal and stays put.
        if agent.status == Status::Waiting {
            let working = looks_working(&tail);
            // Auto-resume purely on the prompt disappearing is safe ONLY for the
            // waits the watcher itself raised (identified by sentinel msg); a real
            // PermissionRequest wait carries the project name and stays until the
            // human answers it (then PostToolUse/`looks_working` resumes it).
            let answered_plan = agent.message == PLAN_APPROVAL_MSG
                && !is_active_plan_prompt(&tail);
            let answered_tool = agent.message == TOOL_APPROVAL_MSG
                && !is_active_tool_prompt(&tail);
            if working || answered_plan || answered_tool {
                let why = if working {
                    "agent working"
                } else if answered_plan {
                    "plan prompt gone"
                } else {
                    "tool prompt gone"
                };
                log(&format!(
                    "RUNNING {} ({}, {}) — approval answered ({})",
                    agent.pane_id, agent.agent_type, agent.location, why
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
///
/// Within the live region, signatures are matched line-anchored (see
/// `line_is_crash_signature`), so an agent describing a crash phrase in its own
/// prose — mid-sentence, or in its `※ recap:` line — is not itself read as a
/// crash (NFR7).
fn looks_crashed(tail: &str) -> bool {
    if AGENT_CHROME.iter().any(|m| tail.contains(m)) {
        return false;
    }
    if live_region(tail).lines().any(line_is_crash_signature) {
        return true;
    }
    looks_like_bare_shell(tail)
}

/// A captured line is a crash/exit signature only when it *leads* with one.
/// Real shell and runtime errors begin the line (the shell writes them at
/// column 0); agents mention the same phrases mid-sentence in their prose and
/// recap, where they never lead. Leading whitespace is ignored so an indented
/// line still matches.
fn line_is_crash_signature(line: &str) -> bool {
    let line = line.trim_start();
    CRASH_SIGNATURES.iter().any(|s| line.starts_with(s))
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

fn is_tool_prompt(tail: &str) -> bool {
    TOOL_PROMPT_MARKERS.iter().any(|m| tail.contains(m))
}

/// True only when a tool-permission prompt is the LIVE prompt — its marker
/// appears in the bottom `LIVE_REGION_LINES`. An answered prompt has fresh agent
/// output below it, so its marker falls outside this window and reads as
/// inactive. Mirrors `is_active_plan_prompt`.
fn is_active_tool_prompt(tail: &str) -> bool {
    is_tool_prompt(&live_region(tail))
}

/// True when the agent's live footer shows it is actively processing a turn (the
/// interrupt hint). Scoped to the LIVE region and matched case-insensitively;
/// used to resume an approval wait answered in the agent's own pane.
fn looks_working(tail: &str) -> bool {
    let region = live_region(tail).to_ascii_lowercase();
    WORKING_MARKERS.iter().any(|m| region.contains(m))
}

/// True when a Claude turn has ended in an unrecoverable API error: an
/// `API Error:` line in the LIVE region while the agent is NOT working. The
/// live-region scope keeps an error scrolled up by later output (a turn that
/// recovered) from matching, and the `!looks_working` guard excludes the
/// transient "Retrying…" state, which keeps the interrupt hint up — so only a
/// terminal error is flagged.
fn looks_errored(tail: &str) -> bool {
    let live = live_region(tail);
    API_ERROR_MARKERS.iter().any(|m| live.contains(m)) && !looks_working(tail)
}

/// The bottom `LIVE_REGION_LINES` of a capture, joined back into one string —
/// after peeling off trailing chrome that Claude pins BELOW a live prompt box:
/// a task-list panel (`trim_trailing_task_panel`) and/or the "run in background"
/// hint footer (`trim_trailing_bg_hints`). Either can push the prompt's markers
/// up out of the fixed bottom window; peeling them drops the prompt back into it.
fn live_region(tail: &str) -> String {
    let mut lines: Vec<&str> = tail.lines().collect();
    // Peel in a loop: the two chrome blocks can stack (in either order) at the
    // bottom, and stripping one can expose the other. Each peel only shrinks the
    // vec, so this reaches a fixed point and terminates.
    loop {
        let before = lines.len();
        trim_trailing_bg_hints(&mut lines);
        trim_trailing_task_panel(&mut lines);
        if lines.len() == before {
            break;
        }
    }
    let start = lines.len().saturating_sub(LIVE_REGION_LINES);
    lines[start..].join("\n")
}

/// Peel Claude's "run in background" hint footer off the bottom of a capture.
///
/// While a foreground command runs — or while its permission prompt is up —
/// Claude pins a `(ctrl+b … to run in background)` hint to the very bottom of
/// the pane, sometimes several lines of it (observed as four lines under a
/// 5-agent run). Like the task panel, that footer pushes a live prompt's markers
/// up out of the bottom `LIVE_REGION_LINES` window, so an un-hooked tool/plan
/// prompt went undetected and the pane stayed RUNNING instead of flipping to
/// WAITING: the reported gap where a `curl` command-safety confirmation, rendered
/// below the prompt's `Esc to cancel …` line, was never seen.
///
/// Conservative like `trim_trailing_task_panel`: strips ONLY a trailing run of
/// hint lines (blank lines between them tolerated); the first non-hint line
/// stops the peel, so a missed strip degrades to the old behavior. It cannot
/// manufacture a false prompt — the hint renders only while a command/prompt is
/// live, and the tool-prompt branch is additionally gated by `!looks_working`.
fn trim_trailing_bg_hints(lines: &mut Vec<&str>) {
    let mut i = lines.len();
    let mut hints = 0usize;
    while i > 0 {
        let line = lines[i - 1].trim();
        if line.is_empty() {
            i -= 1;
            continue;
        }
        if is_bg_hint_line(line) {
            i -= 1;
            hints += 1;
            continue;
        }
        break;
    }
    // Truncate only when we actually saw hints, so a run of trailing blanks alone
    // (or nothing) leaves the capture untouched.
    if hints > 0 {
        lines.truncate(i);
    }
}

/// A "run in background" hint line, e.g. `(ctrl+b ctrl+b (twice) to run in
/// background)`. Anchored to a leading `(` so agent prose that merely mentions
/// running something in the background is not mistaken for the footer chrome.
/// `line` is pre-trimmed.
fn is_bg_hint_line(line: &str) -> bool {
    line.starts_with('(') && line.contains(BG_HINT_MARKER)
}

/// Peel Claude's persistent task-list panel off the bottom of a capture.
///
/// When a task list is active, Claude pins it to the very bottom of the pane —
/// BELOW a live permission/plan prompt box. That panel (an `N tasks (…)` header
/// plus one `◼`/`◻` line per item) pushes the prompt's markers up out of the
/// bottom `LIVE_REGION_LINES` window, so the watcher never saw the prompt and
/// left the pane stuck at RUNNING: the reported gap where an agent with a task
/// list drops out of the attention tier instead of showing WAITING. The panel is
/// persistent chrome, not fresh agent output, so peeling it off before scoping
/// the live region drops the prompt back into it. It also sharpens the answered-
/// prompt check (fresh `⏺` output below a scrolled-up prompt still reads as
/// inactive, because the panel — not that output — is what got trimmed).
///
/// Conservative by construction: we strip ONLY a trailing run of panel lines that
/// terminates in the panel HEADER with at least one item line below it. A run
/// that doesn't (an unrecognized glyph, or a lone header-like line that is really
/// agent output) is left untouched — a missed strip degrades to the old behavior,
/// never a false prompt. Crash/shell detection is unaffected: it reads the full
/// capture, and a crashed pane has no live panel to strip anyway.
fn trim_trailing_task_panel(lines: &mut Vec<&str>) {
    let mut i = lines.len();
    let mut items = 0usize;
    while i > 0 {
        let line = lines[i - 1].trim();
        if line.is_empty() {
            i -= 1;
            continue;
        }
        if is_task_item_line(line) {
            i -= 1;
            items += 1;
            continue;
        }
        // First non-blank, non-item line: strip iff it's the header opening a
        // panel we actually saw items under.
        if items > 0 && is_task_header_line(line) {
            lines.truncate(i - 1);
        }
        return;
    }
}

/// The task-panel header, e.g. `8 tasks (3 done, 1 in progress, 4 open)`: a
/// leading count followed by the `task(s) (…)` breakdown. `line` is pre-trimmed.
fn is_task_header_line(line: &str) -> bool {
    let counted = line
        .split_whitespace()
        .next()
        .is_some_and(|w| w.parse::<u32>().is_ok());
    counted && (line.contains("tasks (") || line.contains("task (")) && line.contains(')')
}

/// A line inside the task panel: an item led by a status glyph, or the
/// `… +N completed` continuation Claude renders when the list is truncated.
/// `line` is pre-trimmed.
fn is_task_item_line(line: &str) -> bool {
    line.starts_with('…')
        || line.starts_with("...")
        || TASK_GLYPHS.iter().any(|g| line.starts_with(g))
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
    fn agent_recap_mentioning_a_crash_phrase_is_not_a_crash() {
        // Reported false positive: the agent FIXED a `command not found` bug and
        // said so — in its end-of-turn summary AND its `※ recap:` line, both of
        // which land in the live region. A bare-substring match on
        // "command not found" read that prose as the agent itself crashing. The
        // phrase never LEADS a line here (it sits mid-sentence / in backticks),
        // so line-anchored matching must not flag it.
        let tail = "\u{23fa} Fixed and verified. uv run spookey now resolves (exit 0) — \
                    the command not found failure at utils.sh:15 is gone.\n\
                    \n\
                    \u{273b} Cooked for 4m 45s\n\
                    \n\
                    \u{203b} recap: Goal: get run_local.sh working. Fixed the \
                    `spookey: command not found` error by using `uv run spookey`.\n\
                    \x20\x20Next: re-run the experiment, then commit. (disable recaps in /config)";
        assert!(tail.contains("command not found")); // present in the live region…
        assert!(!looks_crashed(tail)); // …but only as prose, never leading a line
    }

    #[test]
    fn real_shell_command_not_found_is_still_a_crash() {
        // A genuine drop-to-shell: the shell writes the not-found error at the
        // start of the line. The `bash:`/`zsh:` prefixes (which lead such lines)
        // catch it, so dropping the bare "command not found" signature loses no
        // real detection.
        assert!(looks_crashed("running the agent\nbash: spookey: command not found"));
        assert!(looks_crashed("running the agent\nzsh: command not found: spookey"));
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
        // snap back to WAITING.
        let mut lines = vec!["Ready to code?", "❯ 1. Yes", "  2. No, keep planning"];
        for _ in 0..LIVE_REGION_LINES {
            lines.push("⏺ working on the plan…");
        }
        let tail = lines.join("\n");
        assert!(is_plan_prompt(&tail)); // still present in the full capture
        assert!(!is_active_plan_prompt(&tail)); // …but no longer the live prompt
    }

    #[test]
    fn detects_tool_prompt() {
        assert!(is_tool_prompt("Bash command\n  ls\nDo you want to proceed?"));
        // Must NOT collide with the plan prompt's distinct wording.
        assert!(!is_tool_prompt("Would you like to proceed?"));
        assert!(!is_tool_prompt("just some normal output"));
    }

    #[test]
    fn active_tool_prompt_detected_when_box_is_at_bottom() {
        // The reported gap: a Bash command-safety confirmation (brace-expansion
        // warning) pinned to the bottom of the pane. No hook fired, so the
        // watcher must catch it. Also proves it is neither working nor crashed.
        let tail = "earlier agent output\n\
                    Bash command\n\
                    \x20\x20gh auth status | grep account; git log @{u}..\n\
                    \x20\x20Run shell command\n\
                    Brace expansion (unquoted `{` in concatenation with `,`/`..`)\n\
                    Do you want to proceed?\n\
                    ❯ 1. Yes\n\
                    \x20\x202. No\n\
                    Esc to cancel · Tab to amend · ctrl+e to explain";
        assert!(is_active_tool_prompt(tail));
        assert!(!looks_working(tail)); // footer is "esc to cancel", not "interrupt"
        assert!(!looks_crashed(tail)); // a prompt is not a shell/crash
    }

    #[test]
    fn answered_tool_prompt_in_scrollback_is_not_active() {
        // The prompt was answered and the agent resumed; its marker has scrolled
        // up out of the live region even though it remains in the wider capture.
        // This is what lets the watcher auto-resume a `TOOL_APPROVAL_MSG` wait.
        let mut lines = vec!["Do you want to proceed?", "❯ 1. Yes", "  2. No"];
        for _ in 0..LIVE_REGION_LINES {
            lines.push("⏺ running the approved command…");
        }
        let tail = lines.join("\n");
        assert!(is_tool_prompt(&tail)); // still present in the full capture
        assert!(!is_active_tool_prompt(&tail)); // …but no longer the live prompt
    }

    #[test]
    fn tool_prompt_under_a_task_panel_is_still_active() {
        // Reported bug: a Bash command-safety confirmation with Claude's task-list
        // panel pinned BELOW it. The panel pushes "Do you want to proceed?" out of
        // the bottom LIVE_REGION_LINES, so the watcher missed the prompt and the
        // pane dropped out of the attention tier instead of showing WAITING.
        let tail = "earlier agent output\n\
                    Ask rule Bash(python3 -c *) overrides auto mode for this command.\n\
                    /permissions to let auto mode decide\n\
                    \n\
                    Do you want to proceed?\n\
                    ❯ 1. Yes\n\
                    \x20\x202. No\n\
                    \n\
                    Esc to cancel · Tab to amend · ctrl+e to explain\n\
                    \n\
                    \x20\x208 tasks (3 done, 1 in progress, 4 open)\n\
                    \x20\x20◼ Update SageMaker Dockerfile to use uv\n\
                    \x20\x20◻ Update pre-commit config\n\
                    \x20\x20◻ Update README.md and AGENTS.md\n\
                    \x20\x20◻ Update run_local.sh scripts\n\
                    \x20\x20◻ Verify migration end to end\n\
                    \x20\x20\x20… +3 completed";
        // Without the panel-strip the prompt sits ~13 lines up and is missed.
        assert!(is_active_tool_prompt(&tail));
        assert!(!looks_working(&tail)); // footer is "esc to cancel", not "interrupt"
        assert!(!looks_crashed(&tail)); // a prompt under a task list is not a crash
    }

    #[test]
    fn plan_prompt_under_a_task_panel_is_still_active() {
        // Same panel-below-prompt layout, but for a plan-approval prompt (which
        // fires no hook at all, so the watcher is the only detector).
        let tail = "here is the plan\n\
                    Would you like to proceed?\n\
                    ❯ 1. Yes\n\
                    \x20\x202. No, keep planning\n\
                    \n\
                    \x20\x203 tasks (0 done, 1 in progress, 2 open)\n\
                    \x20\x20◼ Draft the plan\n\
                    \x20\x20◻ Get approval\n\
                    \x20\x20◻ Implement";
        assert!(is_active_plan_prompt(&tail));
    }

    #[test]
    fn answered_prompt_under_a_task_panel_stays_inactive() {
        // The prompt was answered: its box is gone and fresh `⏺` output renders,
        // with the task panel pinned below. Stripping the panel reveals the fresh
        // output — not the scrolled-up prompt — so it must NOT re-trigger WAITING.
        let mut lines = vec!["Do you want to proceed?", "❯ 1. Yes", "  2. No"];
        for _ in 0..LIVE_REGION_LINES {
            lines.push("⏺ running the approved command…");
        }
        lines.push("  5 tasks (2 done, 1 in progress, 2 open)");
        lines.push("  ◼ Do the thing");
        lines.push("  ◻ Next thing");
        lines.push("   … +2 completed");
        let tail = lines.join("\n");
        assert!(is_tool_prompt(&tail)); // still in the full capture…
        assert!(!is_active_tool_prompt(&tail)); // …but not live after the panel-strip
    }

    #[test]
    fn task_panel_helpers_recognize_and_reject() {
        assert!(is_task_header_line("8 tasks (3 done, 1 in progress, 4 open)"));
        assert!(is_task_header_line("1 task (0 done, 1 in progress, 0 open)"));
        assert!(!is_task_header_line("Ready to code?"));
        assert!(!is_task_header_line("running 3 tasks now")); // no leading count
        assert!(is_task_item_line("◼ in progress item"));
        assert!(is_task_item_line("◻ open item"));
        assert!(is_task_item_line("… +3 completed"));
        assert!(!is_task_item_line("Do you want to proceed?"));
    }

    #[test]
    fn tool_prompt_under_bg_hint_footer_is_still_active() {
        // Reported bug: a Bash command-safety confirmation whose OWN footer runs
        // long — below "Esc to cancel …" sit four "(ctrl+b … to run in
        // background)" hint lines (one per running agent). Those push "Do you
        // want to proceed?" ~10 lines up, past the bottom LIVE_REGION_LINES=8
        // window, so the watcher missed the prompt and the pane stayed RUNNING
        // instead of showing WAITING. The task-tool spinner ("Running 5 agents…")
        // is at the TOP here, so trim_trailing_task_panel does not apply.
        let tail = "⏺ Running 5 agents… (ctrl+o to expand)\n\
                    \x20\x20\x20└ Farrow structure and fractional delay · 16 tool uses · 97.5k tokens\n\
                    \x20\x20\x20\x20\x20⎿  Web Search: \"Splitting the unit delay\"…\n\
                    \n\
                    \x20Bash command · from the general-purpose agent\n\
                    \n\
                    \x20\x20\x20cd \"/private/tmp/…/scratchpad\"\n\
                    \x20\x20\x20curl -sL \"https://…\" -o lin_freq.html\n\
                    \x20\x20\x20Run shell command\n\
                    \n\
                    \x20Permission rule Bash(curl *) requires confirmation for this command.\n\
                    \n\
                    \x20Do you want to proceed?\n\
                    \x20❯ 1. Yes\n\
                    \x20\x20\x202. Yes, and don't ask again for: echo …\n\
                    \x20\x20\x203. No\n\
                    \n\
                    \x20Esc to cancel · Tab to amend · ctrl+e to explain\n\
                    \x20\x20\x20\x20\x20(ctrl+b ctrl+b (twice) to run in background)\n\
                    \x20\x20\x20\x20\x20(ctrl+b ctrl+b (twice) to run in background)\n\
                    \x20\x20\x20\x20\x20(ctrl+b ctrl+b (twice) to run in background)\n\
                    \x20\x20\x20\x20\x20(ctrl+b ctrl+b (twice) to run in background)";
        assert!(is_active_tool_prompt(&tail));
        assert!(!looks_working(&tail)); // footer is "esc to cancel", not "interrupt"
        assert!(!looks_crashed(&tail)); // a prompt under a hint footer is not a crash
    }

    #[test]
    fn answered_prompt_under_bg_hint_footer_stays_inactive() {
        // The prompt was answered: its box is gone and fresh `⏺` output renders,
        // with a background-hint footer pinned below (the approved command is now
        // running in the background). Peeling the hints must reveal that fresh
        // output — not the scrolled-up prompt — so it must NOT re-trigger WAITING.
        let mut lines = vec!["Do you want to proceed?", "❯ 1. Yes", "  2. No"];
        for _ in 0..LIVE_REGION_LINES {
            lines.push("⏺ running the approved command…");
        }
        lines.push("  (ctrl+b to run in background)");
        lines.push("  (ctrl+b to run in background)");
        let tail = lines.join("\n");
        assert!(is_tool_prompt(&tail)); // still in the full capture…
        assert!(!is_active_tool_prompt(&tail)); // …but not live after the hint-strip
    }

    #[test]
    fn bg_hint_helpers_recognize_and_reject() {
        assert!(is_bg_hint_line("(ctrl+b ctrl+b (twice) to run in background)"));
        assert!(is_bg_hint_line("(ctrl+b to run in background)"));
        // Agent prose mentioning the phrase must not be mistaken for the footer.
        assert!(!is_bg_hint_line("I'll run in background so it doesn't block"));
        assert!(!is_bg_hint_line("Do you want to proceed?"));
    }

    #[test]
    fn trailing_bg_hints_alone_are_peeled() {
        let mut lines = vec![
            "Do you want to proceed?",
            "  (ctrl+b to run in background)",
            "  (ctrl+b to run in background)",
        ];
        trim_trailing_bg_hints(&mut lines);
        assert_eq!(lines, vec!["Do you want to proceed?"]);
    }

    #[test]
    fn lone_header_like_line_is_not_stripped() {
        // A header-like line with no item lines below is treated as ordinary
        // agent output, not a panel — so nothing is trimmed.
        let mut lines = vec!["some analysis", "there are 8 tasks (roughly) to do"];
        let before = lines.clone();
        trim_trailing_task_panel(&mut lines);
        assert_eq!(lines, before);
    }

    #[test]
    fn working_footer_signals_active_turn() {
        // Claude and Codex interrupt-hint footers, matched case-insensitively.
        assert!(looks_working("⏺ Editing files…\n  ✻ Cogitating (12s · esc to interrupt)"));
        assert!(looks_working("working\n  Press Esc to interrupt"));
        assert!(!looks_working("idle\n> "));
    }

    #[test]
    fn api_error_ending_a_turn_is_errored() {
        // Reported bug: a turn died with `API Error:` and Claude ended it (no
        // interrupt hint); the `Cooked for …` footer keeps ticking so it never
        // crashes to a shell nor stalls — it must be flagged.
        let tail = "❯ what's @launchd/ being used for?\n\
                    ⎿  Listed directory launchd/\n\n\
                    ⏺ API Error: Unable to connect to API (ConnectionRefused)\n\n\
                    ✻ Cooked for 17m 37s";
        assert!(looks_errored(tail));
        // It is NOT a shell crash — the Claude TUI is still on screen.
        assert!(!looks_crashed(tail));
    }

    #[test]
    fn api_error_while_retrying_is_not_errored() {
        // A transient error mid-retry keeps the interrupt hint up (the turn is
        // still alive and will recover), so it must not be flagged.
        let tail = "⏺ API Error: Overloaded\n\
                    ✻ Retrying in 8s… (attempt 2/10 · esc to interrupt)";
        assert!(!looks_errored(tail));
    }

    #[test]
    fn api_error_in_scrollback_is_not_errored() {
        // An earlier error the turn recovered from scrolls up out of the live
        // region; fresh agent output below it means the agent kept working.
        let mut lines = vec!["⏺ API Error: Overloaded"];
        for _ in 0..LIVE_REGION_LINES {
            lines.push("⏺ recovered and continued working…");
        }
        let tail = lines.join("\n");
        assert!(!looks_errored(&tail));
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

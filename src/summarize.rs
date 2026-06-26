//! Stable topic summarization (`@agent_topic`).
//!
//! A turn-end hook (Claude/Codex `Stop`, Gemini `AfterAgent`) runs `agentq
//! summarize`, which computes a 3-5 word topic ONCE per session and stores it on
//! the pane. The TUI shows it in the TOPIC column so you can recognize a session
//! at a glance without reading its scrollback (the churning MESSAGE field can't
//! do that — it's overwritten by every hook/action).
//!
//! Design choices that matter:
//!   * **Set-once.** We skip the work whenever `@agent_topic` is already set, so
//!     it's at most one model call per session and the value stays stable.
//!   * **Detached.** The hook must never block the agent, and the summarizer
//!     (`claude -p`) takes seconds. `run` spawns a detached worker in its own
//!     process group and returns immediately; the worker outlives the hook.
//!   * **Approved tool.** Summarization goes through the installed Claude Code
//!     CLI (`claude -p`) using the user's normal account — not a raw API call —
//!     to stay on the sanctioned LLM path.
//!   * **Hook-loop safe.** The worker and the `claude -p` it spawns run with
//!     `TMUX_PANE` removed, so any agentq hooks they trigger no-op (every
//!     subcommand short-circuits without a pane), preventing recursion and
//!     stopping a nested session from clobbering the real pane's status.
//!   * **Uniform input.** We summarize the captured pane tail (works for every
//!     agent type) rather than parsing per-agent transcript formats.

use std::env;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::tmux;

/// Haiku — the cheapest current model; ample for a 3-5 word label.
const MODEL: &str = "claude-haiku-4-5-20251001";
/// Lines of pane tail fed to the summarizer.
const CAPTURE_LINES: u32 = 120;
/// Upper bound on how long we wait for `claude -p` before giving up (a later
/// turn retries, since we only set the topic on success).
const TIMEOUT: Duration = Duration::from_secs(90);
/// Final guard on topic length, matched to the TOPIC column width.
const MAX_CHARS: usize = 40;
/// Cap on words kept from the model's reply.
const MAX_WORDS: usize = 6;

const PROMPT_PREFIX: &str = "Below is recent terminal output from an AI coding agent's session. \
In 3 to 5 words, name the topic or task being worked on, as a short noun phrase a human could \
use to recognize this session at a glance. Reply with ONLY that phrase — no quotes, no \
punctuation, no preamble.";

/// Hook entry point. Fast and non-blocking: bail unless we're in tmux and the
/// topic is still unset, then hand the slow work to a detached worker.
pub fn run(agent_type: &str) -> Result<()> {
    let pane = match env::var("TMUX_PANE") {
        Ok(p) if !p.is_empty() => p,
        _ => return Ok(()), // not inside tmux (or a nested summarizer) — no-op
    };

    // Set-once: never recompute or overwrite an existing topic.
    if !tmux::get_pane_option(&pane, "@agent_topic").is_empty() {
        return Ok(());
    }

    spawn_worker(&pane, agent_type)
}

/// Re-exec ourselves as a detached `summarize-worker` so the hook returns at
/// once. New process group + null stdio so it survives the hook's teardown;
/// `TMUX_PANE` removed so the `claude -p` it runs can't fire agentq hooks
/// against this pane.
fn spawn_worker(pane: &str, agent_type: &str) -> Result<()> {
    let exe = env::current_exe().context("locating agentq binary")?;
    Command::new(exe)
        .arg("summarize-worker")
        .args(["--pane", pane, "--type", agent_type])
        .env_remove("TMUX_PANE")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .context("spawning summarize worker")?;
    Ok(())
}

/// Detached worker: capture the pane tail, ask the model for a topic, store it.
/// Best-effort throughout — any failure leaves `@agent_topic` unset so the next
/// turn's hook tries again.
pub fn run_worker(pane: &str, _agent_type: &str) -> Result<()> {
    // Re-check under the worker: a concurrent turn may have set it meanwhile.
    if !tmux::get_pane_option(pane, "@agent_topic").is_empty() {
        return Ok(());
    }

    let tail = tmux::capture_pane(pane, CAPTURE_LINES).unwrap_or_default();
    if tail.trim().is_empty() {
        return Ok(());
    }

    if let Some(topic) = summarize(&tail) {
        if !topic.is_empty() {
            tmux::set_topic(pane, &topic)?;
        }
    }
    Ok(())
}

/// Run `claude -p` on the tail and return a cleaned topic, or `None` on any
/// failure/timeout. Polls `try_wait` so a hung model call is killed rather than
/// leaking a process.
fn summarize(tail: &str) -> Option<String> {
    let prompt = format!("{PROMPT_PREFIX}\n\n{tail}");
    let mut child = Command::new("claude")
        .args(["-p", "--model", MODEL, "--strict-mcp-config", "--output-format", "text"])
        .arg(&prompt)
        .env_remove("TMUX_PANE")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let start = Instant::now();
    loop {
        match child.try_wait().ok()? {
            Some(status) => {
                if !status.success() {
                    return None;
                }
                break;
            }
            None => {
                if start.elapsed() >= TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                thread::sleep(Duration::from_millis(150));
            }
        }
    }

    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    Some(clean_topic(&out))
}

/// Normalize the model's reply into a tidy short phrase: first non-empty line,
/// capped to a few words, then stripped of wrapping quotes/markdown and stray
/// edge punctuation from both ends. (`set_topic` additionally strips
/// tabs/newlines.)
fn clean_topic(raw: &str) -> String {
    let line = raw
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");

    let joined: String = line
        .split_whitespace()
        .take(MAX_WORDS)
        .collect::<Vec<_>>()
        .join(" ");

    // Strip wrapping quotes/backticks/asterisks and edge punctuation, repeatedly
    // from both ends, so e.g. `"Auth refactor".` -> `Auth refactor`.
    let trimmed = joined.trim_matches(|c: char| {
        matches!(c, '"' | '\'' | '`' | '*' | '.' | ',' | '!' | ';' | ':')
    });

    if trimmed.chars().count() > MAX_CHARS {
        trimmed.chars().take(MAX_CHARS).collect()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_quotes_and_trailing_period() {
        assert_eq!(clean_topic("\"Auth refactor\"."), "Auth refactor");
        assert_eq!(clean_topic("`Postgres migration`"), "Postgres migration");
    }

    #[test]
    fn takes_first_nonempty_line_and_caps_words() {
        assert_eq!(clean_topic("\n\n  Topic summary line  \nextra"), "Topic summary line");
        assert_eq!(
            clean_topic("one two three four five six seven eight"),
            "one two three four five six"
        );
    }

    #[test]
    fn strips_markdown_bullet_emphasis() {
        assert_eq!(clean_topic("*TUI topic column*"), "TUI topic column");
    }

    #[test]
    fn empty_reply_yields_empty() {
        assert_eq!(clean_topic("   \n  "), "");
    }
}

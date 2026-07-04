# Agent Triage Dashboard

## Context

Spec: @spec.md

I run several AI coding agents (Claude Code, Codex CLI, and — pending hook support — Gemini
agy CLI) in parallel tmux panes alongside nvim, and I want a **persistent triage dashboard**
that shows the whole fleet, floats the agents that need me to the top, lets me clear them in
place or warp to their pane, and is **still there and current when I glance back** — without
relocating into a separate cockpit (spec §1, NG1).

Approach B is the "build a real project" variant. It keeps the producer (hooks) and the state
model (**tmux pane user-options — tmux is the DB, no SQLite, no separate daemon**), but
replaces the shell + fzf consumer with **one small Rust binary, `agentq`**, that provides:

- a **live-updating** ratatui TUI (agents flip to WAITING in real time, not refresh-on-open) — FR4,
- a full-fleet view with priority sorting/coloring and an optional detail pane (tail of output) — FR2,
- **act-in-place** approve/deny **and** guarded free-text reply, plus exact-pane warp — FR6/FR7/FR8,
- a single distributable binary with no `fzf` dependency,
- a `watch` subcommand for crash/stall detection (FR9) folded into the same binary.

This is deliberately heavier than Approach A. Choose it for the polished live UI and as a
bounded Rust learning project; the data shape (a handful of ephemeral panes) does **not**
need it. The hook contract and state keys are **identical** to Approach A, so the two are
interchangeable at the consumer boundary — you can start with A's shell scripts and later
point the hooks at `agentq status`, or go straight to B.

## The surface: a persistent session, not a popup (FR5)

The load-bearing decision. The spec requires a **persistent surface** that "keeps running when
I switch away … and is still present and current when I switch back" and is explicitly **not**
a modal popup that closes on navigation (FR5; Acceptance #3). OQ-4 leaves the mechanism to this
plan, choosing among _dedicated window / dedicated session / status-line widget_.

**Decision: a dedicated tmux session named `agentq`**, holding one long-lived `agentq tui`
process. It is reachable from anywhere with a single keybind and never dies on navigation, so it
trivially satisfies FR5. One binding (`prefix + i`) **toggles** between my work and the
dashboard; warp (`Enter` in the TUI) switches the client to the agent's **exact pane** (FR8).

`agentq open` centralizes the summon/toggle/return logic (and lazily creates the session) so the
tmux config stays a one-liner and the logic is testable in Rust rather than `if-shell`.

## Architecture

```
 Agent pane (claude/codex/gemini)        Any pane I'm working in
 ┌──────────────────────────┐           ┌──────────────────────────────┐
 │ hook → agentq status      │           │  prefix + i → agentq open     │
 │   WAITING_APPROVAL         │           │   ├─ not in agentq: record    │
 │   ↓ shells out to          │           │   │   origin pane, ensure     │
 │ tmux set -p @agent_*       │◀──reads───│   │   session, switch-client  │
 └──────────────────────────┘  (poll)    │   └─ in agentq: switch-client │
        tmux IS the DB                    │       back to origin pane     │
                                          ╞══════════════════════════════╡
 launchd → agentq watch                   │  session "agentq" (persistent)│
   (scrape panes,                         │   └─ agentq tui  (always on)  │
    set CRASHED/STALLED)                  │      live poll of list-panes  │
                                          │      j/k move · y/n approve   │
                                          │      r reply (guarded) · Enter│
                                          │      warp to exact pane       │
                                          └──────────────────────────────┘
```

`agentq` is one binary with four subcommands sharing a thin `tmux` wrapper module:

| Subcommand                     | Role                                                     | Replaces (Approach A)       |
| ------------------------------ | -------------------------------------------------------- | --------------------------- |
| `agentq status <STATUS> [msg]` | hook target; tags the current pane                       | `bin/agent-status`          |
| `agentq tui`                   | the persistent live dashboard (runs in session `agentq`) | `bin/agent-triage.sh` (fzf) |
| `agentq open`                  | summon/toggle/return binding; lazily creates the session | the `display-popup` binding |
| `agentq watch`                 | resiliency scrape loop (launchd)                         | `watchdog/agent-watch.sh`   |

## State model (unchanged from Approach A)

Pane user-options set on the agent's own pane (`$TMUX_PANE`):
`@agent_status` (RUNNING | WAITING_APPROVAL | IDLE | CRASHED | STALLED),
`@agent_msg`, `@agent_updated` (unix seconds), `@agent_type` (claude | codex | gemini | …).
The pane id is the unique key — dedup is free and state auto-cleans when the pane dies (NFR5).
`@agent_type` is an open string so a third agent slots in without a schema change.

## Project layout (Cargo, in `~/Dev/projects/agent-orchestrate`)

| Path                                     | Role                                                                                                                                                       |
| ---------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `Cargo.toml`                             | crate `agentq`; deps: `ratatui`, `crossterm`, `clap` (derive), `anyhow`, `serde_json` (optional, hook-stdin enrichment)                                    |
| `src/main.rs`                            | `clap` dispatch → `status` / `tui` / `open` / `watch`                                                                                                      |
| `src/tmux.rs`                            | wrapper over `tmux` via `std::process::Command`: `list_panes()`, `set_status()`, `send_keys()`, `send_line()`, `warp()`, `capture_pane()`, session helpers |
| `src/model.rs`                           | `Status` enum (+ spec-ordered priority `Ord`), `Agent` struct, parse a `list-panes` line                                                                   |
| `src/status.rs`                          | `agentq status`: read `$TMUX_PANE`, write the three options in one tmux call                                                                               |
| `src/tui.rs`                             | ratatui app: poll loop, render list (+ optional detail), keybinds, reply composer                                                                          |
| `src/open.rs`                            | `agentq open`: toggle between work and the `agentq` session; lazily create it                                                                              |
| `src/watch.rs`                           | crash/stall detection loop                                                                                                                                 |
| `settings/claude-settings.snippet.json`  | hooks pointing at `agentq status ...`                                                                                                                      |
| `settings/codex-hooks.snippet.toml`      | same for Codex                                                                                                                                             |
| `settings/gemini-hooks.snippet`          | placeholder; wired iff Gemini exposes command hooks (see OQ)                                                                                               |
| `tmux/agent-orchestrate.conf`            | `bind-key i run-shell "agentq open"`                                                                                                                       |
| `launchd/com.mloning.agentq-watch.plist` | runs `agentq watch` (Phase 2)                                                                                                                              |
| `README.md`                              | build + wire-up                                                                                                                                            |

## Phase 1 — `agentq status` + `agentq tui` + `agentq open` (MVP)

**`src/tmux.rs`** — the only place that shells out. Representative shapes:

```rust
pub fn set_status(pane: &str, status: &str, msg: &str, ts: u64) -> anyhow::Result<()> {
    // one tmux invocation, multiple ; -separated commands (NFR3: fast hook)
    Command::new("tmux")
        .args(["set-option","-p","-t",pane,"@agent_status",status, ";",
               "set-option","-p","-t",pane,"@agent_msg",msg, ";",
               "set-option","-p","-t",pane,"@agent_updated",&ts.to_string()])
        .status()?; Ok(())
}

pub fn list_panes() -> anyhow::Result<Vec<Agent>> {
    let out = Command::new("tmux").args(["list-panes","-a","-F",
        "#{pane_id}\t#{@agent_status}\t#{session_name}:#{window_index}\t\
         #{@agent_type}\t#{@agent_updated}\t#{@agent_msg}"])
        .output()?;
    // split lines on '\t' → Agent; drop rows with empty @agent_status
}

// FR8 — land on the EXACT pane, not just the window. A #{pane_id} resolves to
// its session for switch-client and its window for select-window.
pub fn warp(pane_id: &str) -> anyhow::Result<()> {
    Command::new("tmux")
        .args(["switch-client","-t",pane_id, ";",
               "select-window","-t",pane_id, ";",
               "select-pane","-t",pane_id])
        .status()?; Ok(())
}

// FR7 — free-text. `--` stops flag parsing so a line starting with '-' is literal.
pub fn send_line(pane_id: &str, text: &str) -> anyhow::Result<()> {
    Command::new("tmux")
        .args(["send-keys","-t",pane_id,"--",text, ";",
               "send-keys","-t",pane_id,"Enter"])
        .status()?; Ok(())
}
```

**`src/status.rs`** — hook target, must be fast (<10ms, NFR3). No-op when `$TMUX_PANE` unset.

**`src/model.rs`** — `Status` priority **matches FR3 exactly** (this was wrong in the first
draft): tier order, top to bottom, is
`CRASHED/STALLED → WAITING_APPROVAL → RUNNING → IDLE`.
CRASHED and STALLED share the top tier. Within a tier, **oldest `@agent_updated` first**
(OQ-2 default; tie-break is a one-line change if I later prefer most-recently-changed).

```rust
// smaller sorts higher; (tier, @agent_updated ascending)
fn tier(&self) -> u8 { match self {
    Crashed | Stalled => 0,
    WaitingApproval   => 1,
    Running           => 2,
    Idle              => 3,
}}
```

**`src/tui.rs`** — event loop polls `tmux::list_panes()` every ~500ms (also redraws on key):

- shows the **whole fleet** — every pane with `@agent_status` set, RUNNING and IDLE included
  (FR2) — sorted by the FR3 tier order above, color-coded; each row shows type, state,
  `session:window`, age/time-in-state, and the short message (FR2). It's a live dashboard, not
  just a queue of stalled agents.
- keybinds:
  - `j/k`/arrows — move selection.
  - `y` — `send_keys(pane,"y","Enter")` + set the pane RUNNING; stay in the dashboard, refresh
    next poll (FR6). `n` likewise with `n` (FR6).
  - `r` — **reply / free-text send** (FR7): opens a one-line composer at the bottom; `Enter`
    sends via `send_line(pane, text)`, `Esc` cancels. **Guard:** `r` only opens directly when
    the selected agent is in an attention-state (`WAITING_APPROVAL | STALLED |
CRASHED`). For any non-attention state (RUNNING/IDLE) it first requires a
    confirm (`send to a non-waiting pane? y/N`) so stray input can't be injected into a busy
    agent (FR7 guard; §8 tradeoff).
  - `Enter` — `warp(pane_id)`: switch the client to the agent's **exact pane** (FR8). The
    dashboard process keeps running in its session; I return with `prefix + i`.
  - `q` — does **not** kill the dashboard; it just returns to my origin pane (same as
    `agentq open` from inside the session). The persistent surface stays alive (FR5).
  - optional: a right detail pane shows a `capture_pane` tail of the selected agent.
- Terminal hygiene: enter/leave raw mode + alternate screen via `crossterm`; the process lives
  for the session's lifetime, so the only restore concern is a clean teardown if `agentq tui`
  itself exits (e.g. the session is killed) — restore on every exit path.

**`src/open.rs`** — the summon/toggle/return binding (FR5/FR8 return path):

```text
current = tmux display -p '#{client_session}'
if current == "agentq":
    # I'm looking at the dashboard → go back to where I summoned it from
    switch-client -t  $(tmux show -gv @agentq_origin)
else:
    # ensure the persistent dashboard exists, remember where I am, go to it
    tmux has-session -t agentq || tmux new-session -d -s agentq -n triage "agentq tui"
    tmux set -g @agentq_origin "#{session_name}:#{window_index}.#{pane_index}"
    tmux switch-client -t agentq
```

This gives the spec's "switch away and back" (Acceptance #3) and "return to where I was"
(Acceptance #6) with a single key. Note the documented semantics: `@agentq_origin` tracks the
pane I last summoned the dashboard _from_. After an `Enter`-warp to an agent, that agent's pane
becomes my new "work" location, so a subsequent `prefix + i` toggles dashboard↔agent — exactly
the triage loop I want. (Return-semantics nuance flagged in gotchas to validate by feel.)

**Hook snippets** — identical events to Approach A, command swapped to the binary (FR1):

- Claude (`~/.claude/settings.json`): use the dedicated `PermissionRequest` event →
  `WAITING_APPROVAL` (NOT the generic `Notification`, which also fires `idle_prompt` when an
  agent is merely idle and so mislabels idle agents as waiting — confirmed in practice);
  `UserPromptSubmit` → `RUNNING` (registration, FR1); `Stop` → `IDLE`; `SessionEnd` →
  `agentq clear` (removes the agent on exit). Elicitation/free-text prompts are folded into
  `WAITING_APPROVAL` — there is no separate input state (a dedicated `Notification` matcher
  would share the idle false-positive risk anyway).
- Codex (`~/.codex/config.toml`, `[features] hooks = true` + trust): `PermissionRequest` →
  `WAITING_APPROVAL`; `Stop` → `IDLE` (registration events for Codex, FR1).
- Gemini agy CLI: in scope for v1 (spec §2). Wire the same contract once its first-hook and
  `WAITING_APPROVAL` events are confirmed (spec OQ-5). The state model already accepts `@agent_type
gemini`, so this is a **producer-only** addition with zero consumer change — Gemini ships as a
  fast-follow behind Claude+Codex.

Use an absolute path to the installed `agentq` (or ensure `~/.cargo/bin` is on the hook shell
PATH).

**tmux keybind** (`tmux/agent-orchestrate.conf`, sourced from `~/.tmux.conf`):

```
bind-key i run-shell "agentq open"
```

## Phase 2 — `agentq watch` (resiliency; bolt-on, FR9)

A loop (every ~25s, OQ-1), run via launchd. For each pane with `@agent_status == RUNNING` or a
stale `@agent_updated`: `tmux::capture_pane()` tail → match crash signatures → set `CRASHED`;
`@agent_updated` older than 10 min while RUNNING → `STALLED`. Detection only — never
registration (NG3). Also catches the **plan-approval gap** (Claude's "Accept this plan?" emits
no hook, issue #19283; OQ-3 default _yes_) by matching the prompt text.

**Crash signatures must be fish-aware and must not match the agents' own prompts** (NFR6,
NFR7). My interactive shell is fish, so a bare reborn shell shows the **fish** prompt, not `$ `
— key off the actual fish prompt I use. Critically, **do not** treat `❯` as a crash signal:
Codex (and starship-style prompts) render `❯` while perfectly healthy, so matching it would
flag false CRASHEDs. Combine a positive shell-prompt match with the **absence** of agent UI
chrome, plus language stack-trace signatures (node/python), and tune to keep false positives
rare. Loadable as `launchd/com.mloning.agentq-watch.plist`.

## Implementation status

All four subcommands are built and the project compiles clean (`cargo build`/`clippy`) with
unit tests (`cargo test`, 13 passing) covering the load-bearing logic: FR3 tier ordering +
within-tier oldest-first, `list-panes` line parsing, and the watch heuristics (bare-fish-shell
→ CRASHED, healthy `❯` → **not** crashed, a crashed Codex with `❯` still in scrollback → CRASHED,
stack-trace + chrome gating, plan-prompt match).

- **`status`** — done; round-trip verified against tmux, `<10ms` single tmux call, no-op when
  `$TMUX_PANE` is unset, message sanitized (tabs/newlines → spaces) so it can't corrupt the
  tab-delimited `list-panes` row. Agent kind is passed via a `--type` flag (no env var).
- **`tui`** — done (`src/tui.rs`, ratatui). 500 ms poll loop with 100 ms event tick; full-fleet
  table (type/state/`session:window`/age/message), FR3-sorted and color-coded; `j/k/g/G`,
  `y`/`n` (act + optimistic RUNNING), guarded `r` reply (`ConfirmSend` for non-attention panes),
  `Enter` warp, `d` detail (tail of `capture-pane`), `q`/`Esc` return-to-origin (persistent —
  does **not** quit). `Ctrl-C` is the explicit teardown/exit. Selection is tracked by `pane_id`,
  so the cursor follows its agent as the sorted list reshuffles — verified live (a RUNNING agent
  kept the cursor when another flipped to CRASHED and rose above it).
- **`open`** — done; returns via `warp(@agentq_origin)` for exact-pane landing, and swallows a
  dead-origin error so the keybind never pops a tmux error.
- **`watch`** — done (`src/watch.rs`). **Refinement vs the prose above:** STALLED is detected by
  the captured tail being **unchanged** for `STALL_SECS` (default 600) while RUNNING — not by a
  stale `@agent_updated` alone — so a healthy long-running agent that is actively printing is
  never false-flagged. `❯` is excluded from crash signatures (NFR6) but is **not** treated as
  proof-of-alive, so a crashed Codex sitting at a fish shell is still detected. Signature lists
  (`AGENT_CHROME`, `CRASH_SIGNATURES`, `PLAN_PROMPT_MARKERS`) are `const`s at the top of the
  module, meant to be tuned to the actual fish prompt in use.

**Hook wiring:** `scripts/install-claude-hooks.sh` safely merges the Claude hooks into
`~/.claude/settings.json` — it follows symlinks (writing through to a dotfiles-repo target while
preserving the symlink), is idempotent (replaces our own prior hooks even across command-format
changes), backs up, validates JSON, and writes atomically. Each hook command is fire-and-forget
(`… 2>/dev/null || true`) so a missing/failing `agentq` never blocks or clutters the agent.

**Remaining (as planned, not blockers):** Gemini producer hooks (OQ-5, fast-follow — consumer
is already type-agnostic); user-side wiring (`cargo install --path .`, source the tmux conf,
run the installer, `launchctl load` the plist); and the by-feel interactive validations
below (warp precision, toggle/return cadence, hook latency) that need the live multi-agent
environment.

## Known risks / gotchas to validate during build

- **Persistent session vs popup** — confirm `agentq tui` in session `agentq` survives
  switching to another session and back, still current (FR5/Acceptance #3). This replaces the
  old popup's terminal-restore worry; the new concern is just clean teardown if the session is
  killed.
- **Toggle/return feel** — `agentq open`'s `@agentq_origin` semantics (above) are the load-
  bearing UX. Validate the full loop by feel: summon → glance → back; summon → `Enter` warp →
  `prefix+i` back. If "return to the _pre-dashboard_ pane after a warp" turns out to matter,
  store a small origin stack instead of a single global option (OQ-R).
- **Exact-pane warp** — verify `warp(pane_id)` lands on the precise pane (not just the window),
  and that a `#{pane_id}` is an acceptable target for `switch-client`/`select-window`/
  `select-pane` on this tmux version (FR8).
- **Free-text guard** — verify `r` is blocked/confirmed for RUNNING/IDLE rows and flows
  straight through for attention-states (FR7).
- **`$TMUX_PANE` in hook env / `agentq status` speed** — confirm the pane id is present and the
  three options land in one tmux call, perceived-instant (NFR3). Note the hook does two
  `exec`s (agentq → tmux); measure, since this is the one spot the "right-sized binary" costs
  more than a popup-free shell would.
- **Distribution / PATH** — `cargo install --path .` puts `agentq` in `~/.cargo/bin`; ensure
  that dir is on PATH for interactive use, the hook shell, **and** the `run-shell` binding.
- **Codex hook constraints** — only `type:"command"` handlers run today; hooks must be trusted.
- **Gemini hooks** — confirm its event→state mapping (spec OQ-5) before wiring; the rest of the
  system is type-agnostic, so only `settings/gemini-hooks.snippet` changes.
- (Go is a viable alternative to Rust here — same design, `bubbletea` instead of `ratatui`.
  Defaulting to Rust per the stated learning goal and the original plan.)

## Open questions (this plan)

- **OQ-R — Return semantics after a warp:** single `@agentq_origin` (current design) vs an
  origin stack. _Default: single option; revisit if it bites._
- Inherits spec OQ-1 (thresholds/cadence), OQ-2 (within-tier tie-break — defaulted to
  oldest-first above), OQ-3 (plan-approval gap — defaulted yes), and OQ-5 (Gemini hook
  mapping — Claude+Codex first, Gemini fast-follow).

## Verification (end-to-end) — mapped to spec Acceptance Criteria

Each must pass for both a Claude and a Codex agent (parity, spec §9).

1. `cargo install --path .`; confirm `agentq --help` shows `status`, `tui`, `open`, `watch`.
2. **Appear (AC1 · FR1/FR4):** wire hooks, start an agent, have it act; within one ~500ms cycle
   it shows in the dashboard. `tmux show-options -p -t <pane> -v @agent_status` reflects state.
3. **Block + rise (AC2 · FR2/FR3/FR4):** trigger a permission prompt → row flips to
   `WAITING_APPROVAL` and rises to its tier. Then kill another agent to a shell (after Phase 2)
   → `CRASHED` sorts **above** the waiting one, confirming the FR3 order.
4. **Persist (AC3 · FR5):** with the dashboard up, `prefix+i` to my work, do something, `prefix+i`
   back → the dashboard is **still running** and current, not reopened.
5. **Act in place (AC4 · FR6):** `y` → the agent's pane receives `y⏎` and proceeds; row drops to
   RUNNING. Repeat with `n`. Dashboard stays put throughout.
6. **Reply (AC5 · FR7):** select a `WAITING_APPROVAL` agent, `r`, type a line, `Enter` → received.
   Then try `r` on a RUNNING row → blocked/confirmed, no stray input sent.
7. **Jump (AC6 · FR8):** `Enter` on a row → client lands on that agent's **exact pane**;
   `prefix+i` returns to where I was.
8. **Crash (AC7 · FR9/FR3):** `agentq watch` running; kill an agent to a bare fish shell → flips
   to `CRASHED` within ~25s, sorts to the top, and a healthy Codex (`❯` prompt) is **not** false-
   flagged.
9. **Codex/Gemini parity:** repeat 2–8 for Codex; for Gemini once OQ-G is resolved.

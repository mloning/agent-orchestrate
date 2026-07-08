# Spec — Agent Triage Dashboard

**Status:** draft for review · **Owner:** mloning · **Date:** 2026-06-23

This document defines **what** we are building and **why**. It is deliberately
**approach-agnostic**: it does not pick a UI toolkit, a state store, or a process model.
When this spec and an approach/plan doc disagree, this spec wins.

---

## 1. Problem

I run several AI coding agents (Claude Code, Codex CLI, Gemini agy CLI) in parallel, each in its own tmux
pane alongside my nvim editor and shells. Three things cost me time, in priority order:

1. **Missed approval prompts** — an agent sits blocked on a `y/n` and I don't notice.
2. **Context-switch cost** — hopping between sessions to approve trivial prompts wrecks focus.
3. **Silent crashes/stalls** — an agent dies to a bare shell or hangs, and I find out late.

Existing multi-agent tools (Claude Squad, NTM, Agent Teams) assume I **live inside their
cockpit**, which pulls me out of my own tmux + nvim environment. The gap is specific: I want
a **persistent triage surface** that shows every agent's state, alerts me when one needs me,
and lets me act or jump to its tmux session — without relocating into a separate app.

A survey of existing tools against these requirements is in **§12** — no off-the-shelf tool
covers the spec. The defining gaps that hold across all of them are the crash/stall watcher
(FR9), arbitrary free-text reply (FR7), and hook-driven **and** pane-keyed registration (FR1).

## 2. Users & environment

- **User:** single developer (me), single machine, local only.
- **Platform:** macOS (darwin); login shell is **fish**; primary editor nvim; all agents run
  inside **tmux** panes, inside a tmux session.
- **Agents (v1):** Claude Code, Codex CLI, and Gemini agy CLI — **all three are in scope for
  v1**. The design should extend to other CLI agents that can emit hooks. Gemini's hook surface
  is less established than Claude's/Codex's, so its registration event(s) are pending
  confirmation (OQ-5); since an agent with no usable hook is invisible (FR1/NG3 — the watcher
  does not register it), Gemini stays out only if it cannot emit any usable hook.

## 3. Goals — what success looks like

A persistent, always-current view of my whole agent fleet that:

- ranks the fleet so the most urgent agents (waiting, crashed/stalled) are at the top
  while everything else stays visible — so a glance tells me what's running and who needs me;
- lets me clear a blocked agent (approve/deny, or type a reply) **without leaving the view**;
- lets me jump straight to the agent's tmux pane when I want the full tmux session;
- never makes me babysit or poll it — it updates itself and is still there, up-to-date, when I
  glance back after working elsewhere.

## 4. Non-goals (explicitly out of scope)

- **NG1** — Replacing tmux/nvim with a separate cockpit app. The tool works with tmux and can run
  inside tmux (separate session).
- **NG2** — Modifying, patching, or wrapping the agent binaries/CLIs themselves.
- **NG3** — Auto-discovering agents that have never fired a hook (see §7, registration is
  hook-driven for v1).
- **NG4** — Managing agents outside tmux (GUI/desktop-app sessions, remote hosts).
- **NG5** — Orchestrating, spawning, or assigning work to agents. This is **triage and
  visibility**, not a scheduler.
- **NG6** — Long-term history/metrics or cross-reboot persistence. State is live and tied to
  running panes.
- **NG7** — Any network egress or external hosting. Everything stays local.

## 5. Domain model — agent states

| State              | Meaning                                    | Attention? |
| ------------------ | ------------------------------------------ | ---------- |
| `RUNNING`          | actively working                           | no         |
| `IDLE`             | finished its turn, awaiting next prompt    | no         |
| `WAITING` | blocked on a prompt (`y-n` permission or free-text) | **yes**    |
| `STALLED`          | RUNNING but no progress past a threshold   | **yes**    |
| `CRASHED`          | died to a bare shell / fatal error         | **yes**    |

- **Attention-states** = `{WAITING, STALLED, CRASHED}`. These float to
  the top of the dashboard (FR3); there are **no active alerts** — noticing is by glance.
- An agent is identified by its **tmux pane**. When the pane dies, its entry disappears (no
  stale rows).

## 6. Functional requirements

Numbered for traceability — the implementation plan should map each step to an FR.

- **FR1 — Registration (hook-driven).** An agent is registered/visible upon its **first hook
  event** (Claude `UserPromptSubmit`; Codex `PermissionRequest`/`Stop`; Gemini agy CLI's
  equivalent first hook event — pending confirmation, OQ-5). Its identity is its tmux pane.
  _Accepted limitation in §8._
- **FR2 — Full-fleet visibility.** The dashboard shows **all** registered agents at once;
  attention-states must not hide the rest. Each row shows at least: agent type
  (claude/codex/gemini), state, location (`session:window`), age/time-in-state, and a short message.
- **FR3 — Priority ordering.** Rows are sorted by tier, top to bottom:
  `CRASHED/STALLED` → `WAITING` → `RUNNING` → `IDLE`.
  Within a tier, oldest-waiting first (tie-break; see OQ-2).
- **FR4 — Live updates.** The view reflects state changes automatically and near-real-time —
  no manual refresh, no reopening to see current state.
- **FR5 — Persistence.** The dashboard is a **persistent surface**: it keeps running when I
  switch away to another tmux session/pane, and is still present and current when I switch
  back. It is **not** a modal/ephemeral popup that closes on navigation.
- **FR6 — Quick actions (approve/deny).** From the dashboard I can approve (`y⏎`) or deny
  (`n⏎`) the selected agent's pending prompt **without leaving the dashboard**.
- **FR7 — Free-text send.** From the dashboard I can type an arbitrary line and send it to the
  selected agent's pane. This **must guard** against sending to a pane that isn't awaiting
  input (e.g. require an attention-state, or confirm) to avoid injecting stray input.
- **FR8 — Jump/warp.** From a row I can jump directly to the **exact pane** where that agent
  runs (not just its window), and return to where I was.
- **FR9 — Crash/stall detection.** A lightweight background **watcher** detects crashes (agent
  dropped to a shell / fatal error signature) and stalls (no progress past a threshold) and
  sets `CRASHED`/`STALLED`, because **no hook fires for these**. The watcher does _detection
  only_ — not registration (NG3).

> **No alerts.** There is deliberately no active notification (bell/flash/popup). The
> dashboard is passive: attention-states are surfaced purely by floating to the top of the
> always-on view (FR3, FR5). Awareness relies on me glancing at the dashboard.

## 7. Non-functional requirements & constraints

- **NFR1 — No CLI modification.** Integrate only via the agents' **supported hooks** + tmux.
  No edits to the claude/codex binaries or their core config beyond hook registration.
- **NFR2 — Stay in my environment.** Must operate within my existing tmux + nvim setup; never
  require relocating into a separate cockpit (ties to NG1).
- **NFR3 — Fast hooks.** Hook handlers must be effectively instantaneous (target <10ms
  perceived) so they don't slow the agents down.
- **NFR4 — Local & private.** Single-user, single-machine, local only; no network egress.
- **NFR5 — Self-cleaning identity.** Agents are keyed by tmux pane; dead panes must not leave
  stale entries; duplicate entries must not appear.
- **NFR6 — Shell-agnostic / fish-aware.** Scripts must not assume bash/zsh interactively (my
  shell is fish); crash detection must account for the actual prompt(s) in use.
- **NFR7 — Cheap, low-false-positive watcher.** The watcher runs on an interval and must keep
  CPU negligible and false crash/stall flags rare.

## 8. Accepted tradeoffs & known limitations

- **Hook-driven registration (FR1).** An agent is **invisible until its first hook fires**. In
  particular, a Codex agent that is working but hasn't hit a permission prompt won't show as
  RUNNING until it prompts or stops. Accepted for simplicity; revisit if it bites.
- **Gemini hook surface (FR1).** Gemini agy CLI's hooks are less established than Claude's or
  Codex's. Which events it emits — and therefore which transitions (`RUNNING`/`IDLE`/
  `WAITING_*`) it can report — is to be confirmed (OQ-5). If it emits only some, Gemini is
  still registered/visible but with thinner state fidelity; crash/stall remains covered by the
  watcher (FR9), the same as for the others.
- **Heuristic crash/stall (FR9).** Detection by scraping pane output is inherently heuristic —
  expect to tune signatures and thresholds; some false positives/negatives are acceptable.
- **Free-text send risk (FR7).** Sending to a not-ready pane can inject stray input; mitigated
  by the FR7 guard, not eliminated.

## 9. Acceptance criteria

End-to-end scenarios; each must pass for **each in-scope agent — Claude, Codex, and Gemini agy
CLI** (parity). Gemini parity is gated on its hook support (OQ-5): until confirmed, Claude +
Codex parity is the v1 bar and Gemini is a fast-follow that reuses these same scenarios.

1. **Appear:** start an agent and have it act → it shows up within one update cycle. _(FR1, FR4)_
2. **Block + rise:** trigger a permission prompt → row flips to `WAITING` and rises to
   the correct tier within one update cycle. _(FR2, FR3, FR4)_
3. **Persist:** with the dashboard open, switch to another session and back → it's still open
   and showing current state, not closed. _(FR5)_
4. **Act in place:** approve from the dashboard → agent receives `y⏎` and proceeds; row
   updates. Deny likewise. _(FR6)_
5. **Reply:** free-text send a line to a waiting agent → it's received; attempting to send to a
   non-ready pane is blocked/confirmed. _(FR7)_
6. **Jump:** from a row, jump to the agent's exact pane, then return to the prior pane. _(FR8)_
7. **Crash:** kill an agent to a bare shell → watcher flags `CRASHED` within its interval and it
   sorts to the top. _(FR9, FR3)_

## 10. Open questions

- **OQ-1** — Threshold values: stall = no update for N minutes (default 10); watcher interval
  (default ~25s); live-view refresh cadence (default sub-second). Tune during build.
- **OQ-2** — Within-tier tie-break: oldest-waiting-first (default) vs most-recently-changed.
- **OQ-3** — Should the watcher also cover the **plan-approval gap** (Claude's "Accept this
  plan?" emits no hook — issue #19283) via prompt-text matching? _Default: yes._
- **OQ-4** — Persistent-surface mechanism (dedicated tmux window vs session vs status-line
  widget) is an **approach-level** decision, resolved in the plan, not here.
- **OQ-5** — **Gemini agy CLI hook mapping:** which Gemini hook events map to our states
  (registration / `RUNNING` / `IDLE` / `WAITING`), and whether its
  hook surface is rich enough for full parity. _Default: confirm during build; ship Claude +
  Codex first and add Gemini as a fast-follow._

## 11. Glossary

- **Agent** — a Claude Code, Codex CLI, or Gemini agy CLI process running in a tmux pane.
- **Attention-state** — a state that means the agent needs me (see §5).
- **Registration** — the moment an agent becomes visible in the dashboard (FR1).
- **Warp / jump** — switching the tmux client to the agent's pane (FR8).
- **Hook** — an agent-emitted event (Claude/Codex) that runs a command; our integration seam.
- **Watcher** — a background loop that scrapes pane output to detect crashes/stalls (FR9).

---

## 12. Prior art — existing solutions surveyed

_Informational, not normative (this section does not change any requirement). Multi-source web
survey with adversarial verification, conducted 2026-06-24. Conclusion: **no off-the-shelf tool
covers the spec.** Every candidate addresses a subset; the closest are tmux-native TUIs that
get fleet visibility + jump-to-pane, but the FR9 watcher, FR7 free-text reply, and a combined
hook-driven **and** pane-keyed FR1 are unmet across the board._

### 12.1 Coverage against the functional requirements

Legend: ✅ covered · ⚠️ partial · ❌ absent. FR2 (full-fleet visibility) is met at a basic level
by every listed tool, so it is folded into FR4/FR5 rather than given its own column.

| Tool | Agents (CC/Codex/Gemini) | FR1 hook + pane-keyed | FR3 attention float | FR4 live | FR5 persistent in-tmux | FR6 approve/deny | FR7 free-text | FR8 jump-to-pane | FR9 crash/stall watcher |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| **tmuxcc** (nyanko3141592) | ✅ +OpenCode | ⚠️ pane-keyed but **poll-based**, not hooks | ⚠️ states shown, no doc'd float | ✅ ~500ms poll | ✅ tmux-native TUI | ✅ `y`/`n` | ⚠️ numbered choices only (`1-9`), no arbitrary text | ✅ `f`/`F` focus pane | ❌ |
| **marmonitor** (mjjo16) | ✅ | ⚠️ **process-based**, not pane | ✅ permission floated #1 | ✅ 2s daemon | ⚠️ status-bar + modal popup | ❌ read-only | ❌ | ✅ `Opt+1–5`/`prefix+j` | ⚠️ stall yes; crash inferred |
| **ntm** (Named Tmux Manager) | ✅ | ❌ spawns labeled panes, not hook-keyed | ❌ | ⚠️ `--watch` (human-driven) | ✅ tmux-native | ❌ (refuted 0–3) | ❌ | ✅ `ntm zoom`/`attach` | ❌ |
| **Claude Squad** | ✅ +Aider | ❌ 500ms poll, not hooks | ❌ flat list | ⚠️ status only | ✅ Bubbletea TUI | ⚠️ blanket `--autoyes` only | ❌ | ❌ | ❌ |
| **uzi** | ✅ | ❌ session-per-agent | ❌ recency-sorted | ✅ `ls -w` 1s | ❌ no persistent dash | ❌ | ⚠️ broadcast-to-**all** | ❌ orphaned alias | ❌ stall branch is dead code |
| **Claude Code Agent Teams** | ❌ Claude-only | n/a | ❌ *hides* idle (inverse) | ✅ | ❌ 1 team/session, dies w/ session | ⚠️ bubbles to lead | ❌ | ⚠️ split-pane view | ❌ |
| **disler multi-agent-observability** | ❌ Claude-only | ⚠️ hook-driven but keyed to `session_id`, **not pane** | ❌ chrono timeline | ✅ WebSocket | ❌ **separate web cockpit** (violates NG1) | ❌ | ❌ | ❌ | ❌ |

### 12.2 What the survey establishes

- **tmuxcc is the closest single match** — tmux-native TUI, pane-keyed tree, ~500ms live poll,
  approve/deny + numbered choices, focus-to-pane. It misses FR7 (no arbitrary free-text), FR9
  (no crash/stall watcher), and its FR1 is poll-based rather than hook-driven. (Rust, MIT, v0.1.5.)
- **marmonitor is the closest on FR3/FR9** — it actually floats `permission` to the top and
  detects stalls, but it is read-only (no FR6/FR7) and binds to processes, not panes (FR1/NFR5 risk).
- **The gaps that hold across _every_ tool:**
  1. **FR9 — autonomous crash/stall watcher.** No tool ships a pane-output scraper for
     crash-to-bare-shell; this is structural, because **no hook fires for a crash or stall**
     (Claude's `StopFailure` only covers API errors). Detection must be derived externally from tmux.
  2. **FR7 — arbitrary free-text reply.** Tools offer at most `y/n` + numbered choices (tmuxcc)
     or broadcast-to-all (uzi); none send a guarded free-text line to one selected pane.
  3. **FR1 — hook-driven _and_ pane-keyed.** Poll-based tools (tmuxcc) are pane-keyed but not
     hook-driven; the one hook-driven tool (disler) is keyed to `session_id`, not a tmux pane.
     Nobody combines both. Claude Code hooks emit no tmux pane field — the established workaround
     is to read `$TMUX_PANE` from the hook's own environment at fire time.

### 12.3 The spec is buildable from existing primitives (high confidence)

- **Claude Code hooks** cover registration + attention-states: `SessionStart`→register;
  `Notification`(`permission_prompt`/`idle_prompt`)→`WAITING`;
  `Stop`/`SessionEnd`→`IDLE`/gone. _Caveat:_ the documented `notification_type` field is missing
  in practice (GitHub #11964, closed "not planned") — **match on message text**, not the field.
- **tmux** supplies what hooks can't: `capture-pane -p -S/-E` reads any pane (even detached —
  verified on tmux 3.4) for the FR9 watcher; `pipe-pane -O` push-streams a pane; control-mode
  `%output` is async per-pane (but scoped to the attached session only).

### 12.4 Caveats & not characterized

- **Not adversarially verified** (surfaced but outside the verified set) — worth a direct read
  before building, and several relate to OQ-5: `agent-dashboard` (bjornjee, Go/Bubbletea, parses
  Claude Code JSONL transcripts, has a phone PWA), `tmux-agent-sidebar` (hiroppy, cross-session
  sidebar), `tmux-agent-status` (samleeney), `tmux-agent-indicator`, `tap-to-tmux`,
  `agent-tmux-manager` (damelLP), `recon` (gavraz).
- **Not characterized** — Vibe Kanban, Conductor, Crystal yielded no verified claims; they are
  broadly GUI/Electron cockpits that would clash with NG1/NFR2 regardless. Anthropic's built-in
  **Agent View** (shipped 2026-05-12) is a single-list in-Claude dashboard, Claude-only and not
  tmux-persistent.
- **Open for v1 (ties to OQ-5):** whether Codex CLI / Gemini CLI expose a Claude-equivalent hook
  surface, or whether their states must come purely from pane-scraping.

### 12.5 Sources (primary unless noted)

- tmuxcc — https://github.com/nyanko3141592/tmuxcc
- marmonitor — https://github.com/mjjo16/marmonitor
- ntm — https://github.com/Dicklesworthstone/ntm
- Claude Squad — https://github.com/smtg-ai/claude-squad
- uzi — https://github.com/devflowinc/uzi
- Claude Code Agent Teams — https://code.claude.com/docs/en/agent-teams
- disler multi-agent-observability — https://github.com/disler/claude-code-hooks-multi-agent-observability
- Claude Code hooks reference — https://code.claude.com/docs/en/hooks
- tmux control mode — https://github.com/tmux/tmux/wiki/Control-Mode
- tmux(1) man page — https://www.man7.org/linux/man-pages/man1/tmux.1.html
- Not-yet-verified candidates: agent-dashboard https://github.com/bjornjee/agent-dashboard ·
  tmux-agent-sidebar https://github.com/hiroppy/tmux-agent-sidebar ·
  tmux-agent-status https://github.com/samleeney/tmux-agent-status ·
  agent-tmux-manager https://github.com/damelLP/agent-tmux-manager ·
  recon https://github.com/gavraz/recon

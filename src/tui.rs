use std::time::{Duration, Instant};

use anyhow::Result;
use ansi_to_tui::IntoText;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Text;
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};
use ratatui::{DefaultTerminal, Frame};

use crate::model::{self, Agent};
use crate::tmux;

/// How often the fleet is re-read from tmux (FR4: live, near-real-time).
const POLL_INTERVAL: Duration = Duration::from_millis(500);
/// How long `event::poll` blocks before we loop to repaint / re-poll.
const EVENT_TICK: Duration = Duration::from_millis(100);
/// Lines of pane output to capture for the preview pane.
const PREVIEW_LINES: u32 = 200;

const HELP: &str =
    "j/k move · y approve · n deny · r reply · ⏎ warp · x clear · d preview · q back · ^C quit";

/// Input mode for the dashboard. `Normal` is the default; the others drive the
/// guarded free-text composer (FR7).
enum Mode {
    Normal,
    /// Confirming a free-text send to a non-attention pane (FR7 guard).
    ConfirmSend,
    /// Composing a free-text line to send to the selected pane.
    Reply { input: String },
}

struct App {
    agents: Vec<Agent>,
    table_state: TableState,
    /// Pane id of the selected agent, tracked across refreshes so the cursor
    /// stays on the same agent even as the sorted list shifts.
    selected_pane: Option<String>,
    show_preview: bool,
    preview: String,
    mode: Mode,
    /// Transient one-line feedback shown in the footer until the next action.
    flash: Option<String>,
    last_poll: Instant,
    should_quit: bool,
}

impl App {
    fn new() -> Self {
        App {
            agents: Vec::new(),
            table_state: TableState::new(),
            selected_pane: None,
            show_preview: true,
            preview: String::new(),
            mode: Mode::Normal,
            flash: None,
            last_poll: Instant::now(),
            should_quit: false,
        }
    }

    // -- data ---------------------------------------------------------------

    /// Re-read the fleet from tmux and reconcile selection + preview.
    fn refresh(&mut self) {
        self.agents = tmux::list_panes();
        self.sync_selection();
        self.update_preview();
    }

    /// Keep the cursor on the same agent across refreshes; clamp otherwise.
    fn sync_selection(&mut self) {
        if self.agents.is_empty() {
            self.table_state.select(None);
            self.selected_pane = None;
            return;
        }
        let idx = self
            .selected_pane
            .as_ref()
            .and_then(|p| self.agents.iter().position(|a| &a.pane_id == p))
            .unwrap_or_else(|| {
                self.table_state
                    .selected()
                    .unwrap_or(0)
                    .min(self.agents.len() - 1)
            });
        self.table_state.select(Some(idx));
        self.selected_pane = Some(self.agents[idx].pane_id.clone());
    }

    fn selected_agent(&self) -> Option<&Agent> {
        self.table_state.selected().and_then(|i| self.agents.get(i))
    }

    /// Refresh the captured tail for the preview pane (no-op when hidden).
    fn update_preview(&mut self) {
        if !self.show_preview {
            return;
        }
        let pane = self.selected_agent().map(|a| a.pane_id.clone());
        self.preview = match pane {
            Some(p) => tmux::capture_pane_ansi(&p, PREVIEW_LINES).unwrap_or_default(),
            None => String::new(),
        };
    }

    // -- navigation ---------------------------------------------------------

    fn move_selection(&mut self, delta: i32) {
        if self.agents.is_empty() {
            return;
        }
        let len = self.agents.len() as i32;
        let cur = self.table_state.selected().unwrap_or(0) as i32;
        let next = (cur + delta).rem_euclid(len) as usize;
        self.select_index(next);
    }

    fn select_index(&mut self, idx: usize) {
        if idx >= self.agents.len() {
            return;
        }
        self.table_state.select(Some(idx));
        self.selected_pane = Some(self.agents[idx].pane_id.clone());
        self.flash = None;
        self.update_preview();
    }

    // -- actions ------------------------------------------------------------

    /// FR6: approve (`y`) or deny (`n`) the selected agent in place, then mark
    /// it RUNNING optimistically so it drops out of the attention tier at once;
    /// the agent's own next hook will correct the state if needed.
    fn act_yes_no(&mut self, key: &str) {
        let Some(agent) = self.selected_agent().cloned() else {
            self.flash = Some("no agent selected".into());
            return;
        };
        let label = if key == "y" { "approved" } else { "denied" };
        let res = tmux::send_keys(&agent.pane_id, key).and_then(|_| {
            tmux::set_status(
                &agent.pane_id,
                "RUNNING",
                label,
                &agent.agent_type,
                &model::now_unix_secs().to_string(),
            )
        });
        self.flash = Some(match res {
            Ok(_) => format!("{label} {} ({})", agent.location, agent.pane_id),
            Err(e) => format!("send failed: {e}"),
        });
        self.refresh();
    }

    /// FR7: open the free-text composer. Attention-states go straight to the
    /// composer; non-attention panes (RUNNING/IDLE) require confirmation first
    /// so stray input can't be injected into a busy agent.
    fn begin_reply(&mut self) {
        match self.selected_agent().map(|a| a.status) {
            None => self.flash = Some("no agent selected".into()),
            Some(s) if s.is_attention() => self.mode = Mode::Reply { input: String::new() },
            Some(_) => self.mode = Mode::ConfirmSend,
        }
    }

    fn send_reply(&mut self, text: &str) {
        if text.is_empty() {
            self.flash = Some("empty reply — nothing sent".into());
            return;
        }
        let Some(agent) = self.selected_agent().cloned() else {
            self.flash = Some("no agent selected".into());
            return;
        };
        let res = tmux::send_line(&agent.pane_id, text).and_then(|_| {
            tmux::set_status(
                &agent.pane_id,
                "RUNNING",
                "reply sent",
                &agent.agent_type,
                &model::now_unix_secs().to_string(),
            )
        });
        self.flash = Some(match res {
            Ok(_) => format!("sent reply to {} ({})", agent.location, agent.pane_id),
            Err(e) => format!("send failed: {e}"),
        });
        self.refresh();
    }

    /// FR8: switch the client to the selected agent's exact pane. The dashboard
    /// process keeps running in its own session; `prefix+i` (or `q`) returns.
    fn warp_selected(&mut self) {
        let Some(agent) = self.selected_agent().cloned() else {
            return;
        };
        if let Err(e) = tmux::warp(&agent.pane_id) {
            self.flash = Some(format!("warp failed: {e}"));
        }
    }

    fn toggle_preview(&mut self) {
        self.show_preview = !self.show_preview;
        self.update_preview();
    }

    /// Manually remove the selected agent from the dashboard (unset its pane
    /// options). For stale rows whose agent exited without firing a clear hook
    /// (e.g. Codex, which has no session-end event, or a hard kill). A still-
    /// live agent re-registers on its next hook.
    fn clear_selected(&mut self) {
        let Some(agent) = self.selected_agent().cloned() else {
            return;
        };
        self.flash = Some(match tmux::clear_status(&agent.pane_id) {
            Ok(_) => format!("cleared {} ({})", agent.location, agent.pane_id),
            Err(e) => format!("clear failed: {e}"),
        });
        self.refresh();
    }

    /// FR5: `q` returns to the summoning pane WITHOUT killing the dashboard, so
    /// the persistent surface stays alive. Mirrors `agentq open` from inside.
    fn return_to_origin(&mut self) {
        if let Ok(origin) = tmux::get_global_option("@agentq_origin") {
            if !origin.is_empty() {
                let _ = tmux::warp(&origin);
            }
        }
    }

    // -- input --------------------------------------------------------------

    fn handle_key(&mut self, key: KeyEvent) {
        // Discriminant tag avoids holding a borrow of `self.mode` across the
        // `&mut self` handler calls in the arms.
        let tag = match self.mode {
            Mode::Normal => 0u8,
            Mode::ConfirmSend => 1,
            Mode::Reply { .. } => 2,
        };
        match tag {
            0 => self.handle_normal(key),
            1 => self.handle_confirm(key),
            _ => self.handle_reply(key),
        }
    }

    fn handle_normal(&mut self, key: KeyEvent) {
        match (key.code, key.modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => self.should_quit = true,
            (KeyCode::Char('q'), _) | (KeyCode::Esc, _) => self.return_to_origin(),
            (KeyCode::Char('j'), _) | (KeyCode::Down, _) => self.move_selection(1),
            (KeyCode::Char('k'), _) | (KeyCode::Up, _) => self.move_selection(-1),
            (KeyCode::Char('g'), _) | (KeyCode::Home, _) => self.select_index(0),
            (KeyCode::Char('G'), _) | (KeyCode::End, _) => {
                self.select_index(self.agents.len().saturating_sub(1))
            }
            (KeyCode::Char('y'), _) => self.act_yes_no("y"),
            (KeyCode::Char('n'), _) => self.act_yes_no("n"),
            (KeyCode::Char('r'), _) => self.begin_reply(),
            (KeyCode::Char('x'), _) => self.clear_selected(),
            (KeyCode::Char('d'), _) => self.toggle_preview(),
            (KeyCode::Enter, _) => self.warp_selected(),
            _ => {}
        }
    }

    fn handle_confirm(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                self.mode = Mode::Reply { input: String::new() }
            }
            // n, Esc, or anything else cancels the send.
            _ => self.mode = Mode::Normal,
        }
    }

    fn handle_reply(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Enter => {
                let input = match std::mem::replace(&mut self.mode, Mode::Normal) {
                    Mode::Reply { input } => input,
                    _ => String::new(),
                };
                self.send_reply(&input);
            }
            KeyCode::Backspace => {
                if let Mode::Reply { input } = &mut self.mode {
                    input.pop();
                }
            }
            KeyCode::Char(c) => {
                if let Mode::Reply { input } = &mut self.mode {
                    input.push(c);
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn ui(f: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Min(0),    // body
            Constraint::Length(1), // footer
        ])
        .split(f.area());

    render_header(f, app, chunks[0]);
    render_body(f, app, chunks[1]);
    render_footer(f, app, chunks[2]);
}

fn render_header(f: &mut Frame, app: &App, area: Rect) {
    let total = app.agents.len();
    let attention = app
        .agents
        .iter()
        .filter(|a| a.status.is_attention())
        .count();
    let text = format!(
        " agentq · {total} agent{} · {attention} need attention",
        if total == 1 { "" } else { "s" }
    );
    let style = if attention > 0 {
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    };
    f.render_widget(Paragraph::new(text).style(style), area);
}

fn render_body(f: &mut Frame, app: &mut App, area: Rect) {
    if app.agents.is_empty() {
        let p = Paragraph::new(
            "No agents registered yet.\n\nStart an agent and trigger a hook (e.g. submit a prompt).",
        )
        .alignment(Alignment::Center)
        .block(Block::default().borders(Borders::ALL).title(" Agents "));
        f.render_widget(p, area);
        return;
    }

    if app.show_preview {
        // Fleet list on top, a live preview of the selected session in the
        // bottom half, like tmux's session-switcher preview.
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(area);
        render_table(f, app, rows[0]);
        render_preview(f, app, rows[1]);
    } else {
        render_table(f, app, area);
    }
}

fn render_table(f: &mut Frame, app: &mut App, area: Rect) {
    let now = model::now_unix_secs();

    let header = Row::new(["TYPE", "STATE", "LOCATION", "AGE", "MESSAGE"])
        .style(Style::default().add_modifier(Modifier::BOLD));

    let rows = app.agents.iter().map(|a| {
        let age = model::humanize_age(now.saturating_sub(a.updated));
        let mut state_style = Style::default().fg(a.status.color());
        if a.status.is_attention() {
            state_style = state_style.add_modifier(Modifier::BOLD);
        }
        Row::new(vec![
            Cell::from(a.agent_type.clone()),
            Cell::from(a.status.label()).style(state_style),
            Cell::from(a.location.clone()),
            Cell::from(age),
            Cell::from(a.message.clone()).style(Style::default().fg(Color::Gray)),
        ])
    });

    let widths = [
        Constraint::Length(7),  // TYPE (claude/codex/gemini)
        Constraint::Length(20), // STATE ("Waiting for approval" = 20)
        Constraint::Length(16), // LOCATION (session:window)
        Constraint::Length(7),  // AGE ("<1 min", "59 min")
        Constraint::Min(10),    // MESSAGE
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(" Agents "))
        // Subtle dark-gray bar rather than a full REVERSED (near-white) row; the
        // `▌` marker already makes the selection obvious. Indexed(237) keeps the
        // per-cell text colors readable (incl. the dim IDLE gray).
        .row_highlight_style(Style::default().bg(Color::Indexed(237)))
        .highlight_symbol("▌ ");

    f.render_stateful_widget(table, area, &mut app.table_state);
}

fn render_preview(f: &mut Frame, app: &App, area: Rect) {
    let title = match app.selected_agent() {
        Some(a) => format!(" preview · {} · {} ", a.agent_type, a.location),
        None => " preview ".to_string(),
    };
    let block = Block::default().borders(Borders::ALL).title(title);
    // Show the tail: keep only the last N captured lines that fit the box so
    // the freshest output is always visible without scroll bookkeeping.
    let inner_h = area.height.saturating_sub(2) as usize;
    let lines: Vec<&str> = app.preview.lines().collect();
    let start = lines.len().saturating_sub(inner_h);
    let tail = lines[start..].join("\n");
    // The capture carries ANSI escapes (`capture-pane -e`); parse them into
    // styled spans so the preview keeps the session's colors. Fall back to raw
    // text if parsing fails.
    let body = tail.into_text().unwrap_or_else(|_| Text::raw(tail.clone()));
    f.render_widget(Paragraph::new(body).block(block), area);
}

fn render_footer(f: &mut Frame, app: &App, area: Rect) {
    let (text, style) = match &app.mode {
        Mode::Reply { input } => (
            format!("reply> {input}\u{2588}"),
            Style::default().fg(Color::Cyan),
        ),
        Mode::ConfirmSend => {
            let loc = app
                .selected_agent()
                .map(|a| a.location.as_str())
                .unwrap_or("?");
            (
                format!("send to non-waiting pane {loc}? (y/N)"),
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            )
        }
        Mode::Normal => match &app.flash {
            Some(msg) => (msg.clone(), Style::default().fg(Color::Cyan)),
            None => (HELP.to_string(), Style::default().fg(Color::DarkGray)),
        },
    };
    f.render_widget(Paragraph::new(text).style(style), area);
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Launch the persistent live TUI dashboard. Runs until Ctrl-C; `q` returns to
/// the work pane without exiting (FR5).
pub fn run() -> Result<()> {
    let mut terminal = ratatui::init();
    let result = run_loop(&mut terminal);
    ratatui::restore();
    result
}

fn run_loop(terminal: &mut DefaultTerminal) -> Result<()> {
    let mut app = App::new();
    app.refresh();

    while !app.should_quit {
        terminal.draw(|f| ui(f, &mut app))?;

        if event::poll(EVENT_TICK)? {
            if let Event::Key(key) = event::read()? {
                // Only react to presses; some terminals also emit Release/Repeat.
                if key.kind == KeyEventKind::Press {
                    app.handle_key(key);
                }
            }
        }

        if app.last_poll.elapsed() >= POLL_INTERVAL {
            app.refresh();
            app.last_poll = Instant::now();
        }
    }

    Ok(())
}

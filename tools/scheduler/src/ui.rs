//! Pure rendering: `draw(frame, state, now)` paints the advisor surface from
//! [`AppState`] + the cached [`World`] recompute. No state mutation here —
//! keys are handled in `app::update`.
//!
//! Layout, top to bottom: setup strip · ranked queue (with score
//! ingredients — the ordering must be explainable, not asserted) · per-
//! bracket summary · status line. Modals: call-picker and `?` help.

use chrono::{DateTime, Local};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph, Row, Table, Wrap},
    Frame,
};

use crate::{
    app::{AppState, Modal, NoticeLevel, PendingStatus, PollHealth},
    conflict::{SetupStatus, UnixMillis},
    model::BracketId,
    world::QueueEntry,
};

const SELECTED: Style = Style::new().add_modifier(Modifier::REVERSED);

pub fn draw(frame: &mut Frame<'_>, state: &AppState, now: UnixMillis) {
    let [setup_area, queue_area, summary_area, status_area] = Layout::vertical([
        Constraint::Length(4),
        Constraint::Min(8),
        Constraint::Length(state.brackets.len() as u16 + 3),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    draw_setup_strip(frame, setup_area, state, now);
    draw_queue(frame, queue_area, state, now);
    draw_summaries(frame, summary_area, state);
    draw_status(frame, status_area, state, now);

    match &state.ui.modal {
        Some(Modal::CallPicker { setup, selected }) => draw_call_picker(frame, state, *setup, *selected),
        Some(Modal::Help) => draw_help(frame),
        None => {}
    }
}

fn draw_setup_strip(frame: &mut Frame<'_>, area: Rect, state: &AppState, now: UnixMillis) {
    let mut spans = Vec::new();
    for setup in state.board.setups() {
        let selected = state.ui.selected_setup == Some(setup.id);
        let (label, style) = match &setup.status {
            SetupStatus::Free => (format!("{}:free", setup.id.0), Style::new().fg(Color::Green)),
            SetupStatus::Called { bracket, set } => {
                let age = state
                    .called_at
                    .get(&(bracket.clone(), set.clone()))
                    .map(|&at| fmt_age(now - at))
                    .unwrap_or_default();
                let overdue = state
                    .called_at
                    .get(&(bracket.clone(), set.clone()))
                    .is_some_and(|&at| now - at > state.config.no_show_secs as i64 * 1000);
                let style = if overdue {
                    Style::new().fg(Color::Red).add_modifier(Modifier::BOLD)
                } else {
                    Style::new().fg(Color::Yellow)
                };
                (format!("{}:called {} {}", setup.id.0, players_for(state, bracket, set), age), style)
            }
            SetupStatus::InProgress { bracket, set } => (
                format!("{}:playing {}", setup.id.0, players_for(state, bracket, set)),
                Style::new().fg(Color::Cyan),
            ),
            SetupStatus::OccupiedExternal { .. } => (format!("{}:ext", setup.id.0), Style::new().fg(Color::Magenta)),
        };
        let style = if selected {
            style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            style
        };
        spans.push(Span::styled(format!(" {label} "), style));
        spans.push(Span::raw("│"));
    }
    let paragraph = Paragraph::new(Line::from(spans))
        .wrap(Wrap { trim: true })
        .block(Block::bordered().title("Setups (digit = pick/select)"));
    frame.render_widget(paragraph, area);
}

fn draw_queue(frame: &mut Frame<'_>, area: Rect, state: &AppState, now: UnixMillis) {
    let header = Row::new([
        "#", "setup", "bracket", "round", "players", "depth", "iron", "unblk", "wait", "poll",
    ])
    .style(Style::new().add_modifier(Modifier::BOLD));
    let rows = state.world.queue.iter().enumerate().map(|(ix, entry)| {
        let setups = entry.candidate_setups.iter().map(|s| s.0.to_string()).collect::<Vec<_>>().join(",");
        let components = &entry.candidate.components;
        let row = Row::new([
            format!("{}", ix + 1),
            setups,
            short_name(&entry.bracket).to_owned(),
            entry.round_text.clone(),
            entry.players.clone(),
            components.depth.to_string(),
            components.ironman.to_string(),
            components.unblock.to_string(),
            fmt_age(components.wait_secs * 1000),
            health_badge(state, &entry.bracket, now),
        ]);
        if ix == state.ui.queue_ix {
            row.style(SELECTED)
        } else {
            row
        }
    });
    let widths = [
        Constraint::Length(3),
        Constraint::Length(6),
        Constraint::Length(14),
        Constraint::Length(18),
        Constraint::Min(24),
        Constraint::Length(5),
        Constraint::Length(4),
        Constraint::Length(5),
        Constraint::Length(7),
        Constraint::Length(6),
    ];
    let title = format!("Call queue ({} ready)", state.world.queue.len());
    let table = Table::new(rows, widths).header(header).block(Block::bordered().title(title));
    frame.render_widget(table, area);
}

fn draw_summaries(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let header = Row::new(["bracket", "left", "path", "ready", "proj finish", ""]).style(Style::new().add_modifier(Modifier::BOLD));
    let rows = state.world.summaries.iter().map(|summary| {
        let projection = match summary.projected_finish {
            Some(at) => fmt_clock(at),
            None if summary.projection_blocked => "starved".to_owned(),
            None => "—".to_owned(),
        };
        let marker = if summary.projection_includes_unstarted {
            "≥ (unstarted)"
        } else {
            ""
        };
        Row::new([
            short_name(&summary.id).to_owned(),
            summary.incomplete_sets.to_string(),
            summary.critical_path.to_string(),
            summary.callable_now.to_string(),
            projection,
            marker.to_owned(),
        ])
    });
    let widths = [
        Constraint::Length(16),
        Constraint::Length(5),
        Constraint::Length(5),
        Constraint::Length(5),
        Constraint::Length(11),
        Constraint::Min(6),
    ];
    let overall = state
        .world
        .overall_projected_finish
        .map(fmt_clock)
        .unwrap_or_else(|| "—".to_owned());
    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::bordered().title(format!("Brackets · overall projected finish {overall}")));
    frame.render_widget(table, area);
}

fn draw_status(frame: &mut Frame<'_>, area: Rect, state: &AppState, now: UnixMillis) {
    let mut spans = Vec::new();
    if state.writes_armed {
        spans.push(Span::styled(" WRITES ARMED ", Style::new().fg(Color::Black).bg(Color::Green)));
    } else {
        spans.push(Span::styled(
            " ADVISOR-ONLY ",
            Style::new().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD),
        ));
    }
    if state.persist_failed {
        spans.push(Span::styled(
            " STATE NOT PERSISTING ",
            Style::new().fg(Color::White).bg(Color::Red).add_modifier(Modifier::BOLD),
        ));
    }

    let healthy = state.brackets.iter().filter(|b| b.health == PollHealth::Ok).count();
    let oldest = state
        .brackets
        .iter()
        .filter_map(|b| b.last_good_poll)
        .map(|at| now - at)
        .max()
        .unwrap_or(0);
    spans.push(Span::raw(format!(
        " polls {healthy}/{} ok, oldest {} ",
        state.brackets.len(),
        fmt_age(oldest)
    )));

    if let Some(sample) = state.clock_offset {
        let age = now - sample.at;
        // The skew warning only fires on a fresh estimate — a stale one just
        // reports with its age.
        let style = if sample.offset_secs.abs() > 60 && age < 5 * 60 * 1000 {
            Style::new().fg(Color::Red).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(Color::DarkGray)
        };
        spans.push(Span::styled(
            format!("│ offset {:+}s ({}) ", sample.offset_secs, fmt_age(age)),
            style,
        ));
    }

    // Reconnect-held writes count as pending — they are in flight from the
    // TO's point of view, just waiting on the network.
    let queued = state.pending_writes.iter().filter(|p| p.status != PendingStatus::Parked).count();
    let parked = state.pending_writes.iter().filter(|p| p.status == PendingStatus::Parked).count();
    if queued + parked > 0 {
        let style = if parked > 0 {
            Style::new().fg(Color::Red)
        } else {
            Style::new().fg(Color::Yellow)
        };
        spans.push(Span::styled(format!("│ writes {queued} pending, {parked} parked "), style));
    }

    if let Some(notice) = state.notices.back() {
        let style = match notice.level {
            NoticeLevel::Info => Style::new().fg(Color::Gray),
            NoticeLevel::Warn => Style::new().fg(Color::Yellow),
            NoticeLevel::Error => Style::new().fg(Color::Red),
        };
        spans.push(Span::styled(format!("│ {} ", notice.text), style));
    }
    spans.push(Span::styled("│ ? help", Style::new().fg(Color::DarkGray)));

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_call_picker(frame: &mut Frame<'_>, state: &AppState, setup: crate::config::SetupId, selected: usize) {
    let area = centered_rect(frame.area(), 70, 60);
    frame.render_widget(Clear, area);

    let empty = Vec::new();
    let candidates: &Vec<QueueEntry> = state.world.per_setup.get(&setup).unwrap_or(&empty);
    let header =
        Row::new(["#", "bracket", "round", "players", "depth", "iron", "unblk", "wait"]).style(Style::new().add_modifier(Modifier::BOLD));
    let rows = candidates.iter().enumerate().map(|(ix, entry)| {
        let components = &entry.candidate.components;
        let row = Row::new([
            format!("{}", ix + 1),
            short_name(&entry.bracket).to_owned(),
            entry.round_text.clone(),
            entry.players.clone(),
            components.depth.to_string(),
            components.ironman.to_string(),
            components.unblock.to_string(),
            fmt_age(components.wait_secs * 1000),
        ]);
        if ix == selected {
            row.style(SELECTED)
        } else {
            row
        }
    });
    let widths = [
        Constraint::Length(3),
        Constraint::Length(14),
        Constraint::Length(18),
        Constraint::Min(24),
        Constraint::Length(5),
        Constraint::Length(4),
        Constraint::Length(5),
        Constraint::Length(7),
    ];
    let title = format!("Call on setup {} — Enter commits, Esc cancels", setup.0);
    let table = Table::new(rows, widths).header(header).block(Block::bordered().title(title));
    frame.render_widget(table, area);
}

fn draw_help(frame: &mut Frame<'_>) {
    let area = centered_rect(frame.area(), 60, 60);
    frame.render_widget(Clear, area);
    let text = [
        "1-9/0     pick a free setup (call picker) / select an occupied one",
        "Enter     commit the selected call (in picker)",
        "p         selected setup: called -> in progress",
        "f         selected setup: free, awaiting remote result",
        "r         selected setup: un-call, set returns to the queue",
        "z         snooze the highlighted queue entry (5m)",
        "Up/Down   move the queue highlight",
        "u         undo the last local action (single level)",
        "q/Ctrl-C  quit",
    ]
    .into_iter()
    .map(Line::from)
    .collect::<Vec<_>>();
    let help = Paragraph::new(text).block(Block::bordered().title("Keys — Esc closes"));
    frame.render_widget(help, area);
}

fn players_for(state: &AppState, bracket: &BracketId, key: &crate::model::SetKey) -> String {
    state
        .brackets
        .iter()
        .find(|b| &b.state.id == bracket)
        .and_then(|b| b.state.sets.iter().find(|s| &s.key == key))
        .map(|set| {
            set.occupants()
                .map(|o| truncate(&o.display_name, 10))
                .collect::<Vec<_>>()
                .join(" v ")
        })
        .unwrap_or_default()
}

fn health_badge(state: &AppState, bracket: &BracketId, now: UnixMillis) -> String {
    let Some(runtime) = state.brackets.iter().find(|b| &b.state.id == bracket) else {
        return String::new();
    };
    match &runtime.health {
        PollHealth::Ok => {
            let stale_after = (state.config.poll_interval_secs * state.config.stale_warn_polls as u64 * 1000) as i64;
            match runtime.last_good_poll {
                Some(at) if now - at > stale_after => "stale".to_owned(),
                _ => String::new(),
            }
        }
        PollHealth::Offline => "OFF".to_owned(),
        PollHealth::Transient => "retry".to_owned(),
        PollHealth::Persistent(_) => "FAIL".to_owned(),
    }
}

/// A centered `pct_x`% × `pct_y`% sub-rect for modals.
fn centered_rect(area: Rect, pct_x: u16, pct_y: u16) -> Rect {
    let [_, vertical, _] = Layout::vertical([
        Constraint::Percentage((100 - pct_y) / 2),
        Constraint::Percentage(pct_y),
        Constraint::Percentage((100 - pct_y) / 2),
    ])
    .areas(area);
    let [_, horizontal, _] = Layout::horizontal([
        Constraint::Percentage((100 - pct_x) / 2),
        Constraint::Percentage(pct_x),
        Constraint::Percentage((100 - pct_x) / 2),
    ])
    .areas(vertical);
    horizontal
}

fn short_name(id: &BracketId) -> &str {
    id.0.rsplit('/').next().unwrap_or(&id.0)
}

fn truncate(name: &str, max: usize) -> String {
    if name.chars().count() <= max {
        name.to_owned()
    } else {
        name.chars().take(max.saturating_sub(1)).chain(['…']).collect()
    }
}

fn fmt_age(millis: i64) -> String {
    let secs = (millis / 1000).max(0);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    }
}

fn fmt_clock(millis: UnixMillis) -> String {
    DateTime::from_timestamp_millis(millis)
        .map(|utc| utc.with_timezone(&Local).format("%H:%M").to_string())
        .unwrap_or_else(|| "?".to_owned())
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{backend::TestBackend, Terminal};

    use super::draw;
    use crate::{
        app::{update, AppState, BracketBootstrap, Msg},
        config::{BracketConfig, BracketMode, SchedulerConfig, SetupId},
        model::BracketId,
        synth::{make_se_bracket, materialize_ids},
    };

    const NOW: i64 = 1_751_000_000_000;

    fn test_state(writes_armed: bool) -> AppState {
        let mut bracket = make_se_bracket(1001, 4);
        bracket.sets = materialize_ids(&bracket.sets, 9000);
        let slug = "tournament/fbr/event/ultimate-singles";
        let config = SchedulerConfig {
            setups: vec![SetupId(1), SetupId(2)],
            brackets: vec![BracketConfig {
                pool: vec![SetupId(1), SetupId(2)],
                ..BracketConfig::new(slug)
            }],
            ..SchedulerConfig::default()
        };
        let boots = vec![BracketBootstrap {
            id: BracketId(slug.to_owned()),
            sets: bracket.sets.clone(),
            groups: vec![bracket.info.clone()],
            mode: BracketMode::Full,
            start_at: None,
            pool: vec![SetupId(1), SetupId(2)],
            duration_prior_secs: 480,
            prior_weight: 4.0,
        }];
        AppState::new(config, writes_armed, boots, NOW)
    }

    fn render(state: &AppState) -> String {
        let backend = TestBackend::new(130, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, state, NOW + 5000)).unwrap();
        let buffer = terminal.backend().buffer();
        let mut out = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                out.push_str(buffer.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "));
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn renders_board_queue_summary_and_banner() {
        let state = test_state(false);
        let text = render(&state);

        assert!(text.contains("ADVISOR-ONLY"), "disarmed banner:\n{text}");
        assert!(text.contains("1:free") && text.contains("2:free"), "setup strip:\n{text}");
        assert!(text.contains("Player 1 vs Player 4"), "queue players:\n{text}");
        assert!(text.contains("depth"), "score ingredient header:\n{text}");
        assert!(text.contains("ultimate-singles"), "bracket short name:\n{text}");
        assert!(text.contains("overall projected finish"), "summary title:\n{text}");
        assert!(text.contains("Call queue (2 ready)"), "queue count:\n{text}");
    }

    #[test]
    fn armed_banner_and_called_setup_render() {
        let mut state = test_state(true);
        update(&mut state, Msg::Key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE)), NOW);
        update(&mut state, Msg::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), NOW);

        let text = render(&state);
        assert!(text.contains("WRITES ARMED"), "armed banner:\n{text}");
        assert!(text.contains("1:called"), "called setup in strip:\n{text}");
        assert!(text.contains("writes 1 pending"), "pending badge:\n{text}");
        assert!(text.contains("Call queue (1 ready)"), "called set left the queue:\n{text}");
    }

    #[test]
    fn call_picker_modal_renders_candidates() {
        let mut state = test_state(false);
        update(&mut state, Msg::Key(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE)), NOW);

        let text = render(&state);
        assert!(text.contains("Call on setup 2"), "picker title:\n{text}");
        assert!(text.contains("Enter commits"), "picker hint:\n{text}");
    }

    #[test]
    fn help_overlay_renders() {
        let mut state = test_state(false);
        update(&mut state, Msg::Key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE)), NOW);

        let text = render(&state);
        assert!(text.contains("snooze the highlighted queue entry"), "help body:\n{text}");
    }

    #[test]
    fn age_and_clock_formatting() {
        assert_eq!(super::fmt_age(5_000), "5s");
        assert_eq!(super::fmt_age(125_000), "2m05s");
        assert_eq!(super::fmt_age(7_320_000), "2h02m");
        assert_eq!(super::fmt_age(-500), "0s");
        assert_eq!(super::truncate("Short", 10), "Short");
        assert_eq!(super::truncate("A very long gamer tag", 10), "A very lo…");
    }
}

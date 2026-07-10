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
    widgets::{Block, Clear, Paragraph, Row, Table, TableState, Wrap},
    Frame,
};

use crate::{
    app::{
        blocked_entries, filtered_roster, find_set_rows, flag_label, picker_rows, reassign_options, report_roster, setups_rows, AppState,
        ListView, Modal, NoticeLevel, PendingStatus, PollHealth, ReassignOption, ReportDraft, ReportStage, SetupsRow, Side,
    },
    conflict::{occupant_keys, BlockReason, BusySource, ConflictKey, SetupStatus, UnixMillis},
    model::{strip_sponsor, BracketId, SetKey},
    world::RolloutRow,
};

const SELECTED: Style = Style::new().add_modifier(Modifier::REVERSED);

/// Renders a table whose `selected` row must stay visible. The scroll offset
/// persists in the [`ListView`] across draws, so the viewport holds still
/// and only moves when the cursor crosses an edge (a fresh offset every
/// frame would pin the selection to the bottom edge instead); the visible
/// row count recorded alongside sizes a PgUp/PgDn jump.
fn render_with_selection(frame: &mut Frame<'_>, area: Rect, table: Table<'_>, selected: usize, view: &ListView) {
    let mut table_state = TableState::default().with_offset(view.scroll.get()).with_selected(Some(selected));
    frame.render_stateful_widget(table, area, &mut table_state);
    view.scroll.set(table_state.offset());
    // Two border rows plus the header; a one-row error on the headerless
    // modals is fine for a page jump.
    view.rows.set(usize::from(area.height.saturating_sub(3)).max(1));
}

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
        Some(Modal::CallPicker {
            setup,
            selected,
            refreshed,
        }) => draw_call_picker(frame, state, *setup, *selected, *refreshed, now),
        Some(Modal::Inspection { selected }) => draw_inspection(frame, state, *selected),
        Some(Modal::Notices { selected }) => draw_notices(frame, state, *selected, now),
        Some(Modal::PendingWrites { selected }) => draw_pending_writes(frame, state, *selected),
        Some(Modal::PlayerFlags { players, selected }) => draw_player_flags(frame, state, players, *selected),
        Some(Modal::Reassign { setup, selected }) => draw_reassign(frame, state, *setup, *selected),
        Some(Modal::Setups { selected }) => draw_setups(frame, state, *selected),
        Some(Modal::FindSet { query, selected }) => draw_find_set(frame, state, query, *selected),
        Some(Modal::Report(draft)) => draw_report(frame, state, draft),
        Some(Modal::Help) => draw_help(frame),
        None => {}
    }
}

fn draw_setup_strip(frame: &mut Frame<'_>, area: Rect, state: &AppState, now: UnixMillis) {
    // With more than one hardware class on the board, each station shows its
    // type's initial (e.g. `7p:free`); a single-type board stays clean.
    let multi_type = state.board.counts_by_type().len() > 1;
    let mut spans = Vec::new();
    for setup in state.board.setups() {
        let selected = state.ui.selected_setup == Some(setup.id);
        let exhausted = state.world.pool_exhausted.contains(&setup.id);
        let number = match (multi_type, setup.setup_type.chars().next()) {
            (true, Some(initial)) => format!("{}{initial}", setup.id.0),
            _ => setup.id.0.to_string(),
        };
        let (label, style) = match &setup.status {
            SetupStatus::Free if exhausted => (
                format!("{number}:done→a"),
                Style::new().fg(Color::Magenta).add_modifier(Modifier::BOLD),
            ),
            SetupStatus::Free => (format!("{number}:free"), Style::new().fg(Color::Green)),
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
                (format!("{number}:called {} {}", players_for(state, bracket, set), age), style)
            }
            SetupStatus::InProgress { bracket, set } => (
                format!("{number}:playing {}", players_for(state, bracket, set)),
                Style::new().fg(Color::Cyan),
            ),
            SetupStatus::OccupiedExternal { .. } => (format!("{number}:ext"), Style::new().fg(Color::Magenta)),
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
        "#", "setup", "bracket", "round", "set", "players", "score", "depth", "iron", "unblk", "wait", "poll",
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
            entry.key.identifier.clone(),
            show_players(state, &entry.players),
            format!("{:.0}", entry.candidate.score),
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
        Constraint::Length(5),
        Constraint::Length(4),
        Constraint::Min(24),
        Constraint::Length(6),
        Constraint::Length(5),
        Constraint::Length(4),
        Constraint::Length(5),
        Constraint::Length(7),
        Constraint::Length(6),
    ];
    let title = format!("Call queue ({} ready)", state.world.queue.len());
    let table = Table::new(rows, widths).header(header).block(Block::bordered().title(title));
    render_with_selection(frame, area, table, state.ui.queue_ix, &state.ui.queue_view);
}

fn draw_summaries(frame: &mut Frame<'_>, area: Rect, state: &AppState) {
    let header =
        Row::new(["bracket", "left", "path", "ready", "est set", "proj finish", ""]).style(Style::new().add_modifier(Modifier::BOLD));
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
        // Duration introspection: the estimate plus how much of it is still
        // prior vs observed ("8m00s ·3" = three real samples blended in).
        let estimate = fmt_age((state.durations.estimate_secs(&summary.id) * 1000.0) as i64);
        let samples = state.durations.sample_count(&summary.id);
        Row::new([
            short_name(&summary.id).to_owned(),
            summary.incomplete_sets.to_string(),
            summary.critical_path.to_string(),
            summary.callable_now.to_string(),
            format!("{estimate} ·{samples}"),
            projection,
            marker.to_owned(),
        ])
    });
    let widths = [
        Constraint::Length(16),
        Constraint::Length(5),
        Constraint::Length(5),
        Constraint::Length(5),
        Constraint::Length(10),
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

    if let Some(pending) = &state.ui.setup_entry {
        spans.push(Span::styled(
            format!(" setup {}_ ", pending.digits),
            Style::new().add_modifier(Modifier::BOLD),
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

    let unread = state.notices.iter().filter(|n| !n.acked && n.level != NoticeLevel::Info).count();
    if unread > 0 {
        spans.push(Span::styled(
            format!("│ {unread} notices (n) "),
            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ));
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

fn draw_call_picker(
    frame: &mut Frame<'_>,
    state: &AppState,
    setup: crate::config::SetupId,
    selected: usize,
    refreshed: bool,
    now: UnixMillis,
) {
    let area = centered_rect(frame.area(), 70, 60);
    frame.render_widget(Clear, area);

    let (rows, from_rollout) = picker_rows(state, setup);
    let header = Row::new(["#", "bracket", "round", "players", "depth", "iron", "unblk", "wait", "proj"])
        .style(Style::new().add_modifier(Modifier::BOLD));
    let table_rows = rows.iter().enumerate().map(|(ix, picker_row)| {
        let row = match picker_row {
            RolloutRow::Call(entry) => {
                let components = &entry.candidate.components;
                Row::new([
                    format!("{}", ix + 1),
                    short_name(&entry.bracket).to_owned(),
                    entry.round_text.clone(),
                    show_players(state, &entry.players),
                    components.depth.to_string(),
                    components.ironman.to_string(),
                    components.unblock.to_string(),
                    fmt_age(components.wait_secs * 1000),
                    components.projected_finish.map(fmt_clock).unwrap_or_default(),
                ])
            }
            RolloutRow::Hold {
                waiting_for,
                projected_finish,
            } => {
                let waiting = waiting_for
                    .as_ref()
                    .map(|key| format!("waiting for R{} {}", key.round, key.identifier))
                    .unwrap_or_else(|| "waiting".to_owned());
                Row::new([
                    format!("{}", ix + 1),
                    "HOLD".to_owned(),
                    String::new(),
                    waiting,
                    String::new(),
                    String::new(),
                    String::new(),
                    String::new(),
                    projected_finish.map(fmt_clock).unwrap_or_default(),
                ])
                .style(Style::new().fg(Color::Cyan))
            }
        };
        if ix == selected {
            row.style(SELECTED)
        } else {
            row
        }
    });
    let widths = [
        Constraint::Length(3),
        Constraint::Length(14),
        Constraint::Length(5),
        Constraint::Min(22),
        Constraint::Length(5),
        Constraint::Length(4),
        Constraint::Length(5),
        Constraint::Length(7),
        Constraint::Length(6),
    ];
    let ranking = if from_rollout {
        let age = state.rollout.as_ref().map(|r| fmt_age(now - r.computed_at)).unwrap_or_default();
        let updated = if refreshed { ", ranking updated" } else { "" };
        format!("rollout {age} old{updated}")
    } else {
        "greedy (rollout pending)".to_owned()
    };
    let title = format!("Call on setup {} [{ranking}] — Enter commits, Esc cancels", setup.0);
    let table = Table::new(table_rows, widths).header(header).block(Block::bordered().title(title));
    render_with_selection(frame, area, table, selected, &state.ui.modal_view);
}

fn draw_inspection(frame: &mut Frame<'_>, state: &AppState, selected: usize) {
    let area = centered_rect(frame.area(), 80, 70);
    frame.render_widget(Clear, area);
    let [list_area, reasons_area] = Layout::vertical([Constraint::Min(5), Constraint::Length(8)]).areas(area);

    let entries = blocked_entries(state);
    let selected = selected.min(entries.len().saturating_sub(1));
    let header = Row::new(["bracket", "round", "players", "blocked by"]).style(Style::new().add_modifier(Modifier::BOLD));
    let rows = entries.iter().enumerate().map(|(ix, (bracket, key))| {
        let reasons = state.world.blocked.get(&(bracket.clone(), key.clone()));
        let summary = reasons
            .map(|list| list.iter().map(reason_tag).collect::<Vec<_>>().join(", "))
            .unwrap_or_default();
        let row = Row::new([
            short_name(bracket).to_owned(),
            format!("R{} {}", key.round, key.identifier),
            players_for(state, bracket, key),
            summary,
        ]);
        if ix == selected {
            row.style(SELECTED)
        } else {
            row
        }
    });
    let widths = [
        Constraint::Length(16),
        Constraint::Length(8),
        Constraint::Min(20),
        Constraint::Min(24),
    ];
    let title = format!("Blocked sets ({}) — Up/Down, Esc closes", entries.len());
    render_with_selection(
        frame,
        list_area,
        Table::new(rows, widths).header(header).block(Block::bordered().title(title)),
        selected,
        &state.ui.modal_view,
    );

    let detail: Vec<Line<'_>> = entries
        .get(selected)
        .and_then(|(bracket, key)| state.world.blocked.get(&(bracket.clone(), key.clone())))
        .map(|reasons| reasons.iter().map(|r| Line::from(reason_line(state, r))).collect())
        .unwrap_or_default();
    frame.render_widget(
        Paragraph::new(detail)
            .wrap(Wrap { trim: true })
            .block(Block::bordered().title("Why")),
        reasons_area,
    );
}

/// Short tag for the row summary column.
fn reason_tag(reason: &BlockReason) -> &'static str {
    match reason {
        BlockReason::ConflictOnlyBracket => "conflict-only",
        BlockReason::Completed => "done",
        BlockReason::RemotelyActive => "remote-active",
        BlockReason::RemotelyCalled => "remote-called",
        BlockReason::AwaitingRemoteCompletion => "awaiting-result",
        BlockReason::SlotsUnresolved => "slots",
        BlockReason::HasPlaceholder => "placeholder",
        BlockReason::BracketHeld => "held",
        BlockReason::BracketNotOpen { .. } => "not-open",
        BlockReason::NoPermittedFreeSetup => "no-setup",
        BlockReason::PlayerBusy { .. } => "busy",
        BlockReason::PlayerResting { .. } => "resting",
        BlockReason::PlayerDeparted { .. } => "departed",
        BlockReason::RestWindow { .. } => "rest",
        BlockReason::PlayerDisqualified { .. } => "dq",
        BlockReason::Snoozed { .. } => "snoozed",
    }
}

/// Full explanation with the correction hint inline.
fn reason_line(state: &AppState, reason: &BlockReason) -> String {
    match reason {
        BlockReason::ConflictOnlyBracket => "conflict-only bracket — feeds the filter, never called from here".to_owned(),
        BlockReason::Completed => "already completed".to_owned(),
        BlockReason::RemotelyActive => "site shows it started — r on its setup re-queues if that's wrong".to_owned(),
        BlockReason::RemotelyCalled => "site shows it called (someone else's call?) — d force-available overrides a player".to_owned(),
        BlockReason::AwaitingRemoteCompletion => "desk finished it; waiting for the server to confirm".to_owned(),
        BlockReason::SlotsUnresolved => "waiting on prerequisite sets to finish".to_owned(),
        BlockReason::HasPlaceholder => "a slot is still a placeholder".to_owned(),
        BlockReason::BracketHeld => "bracket is manually held".to_owned(),
        BlockReason::BracketNotOpen { starts_at } => match starts_at {
            Some(at) => format!("bracket not open yet (starts {})", fmt_clock(at * 1000)),
            None => "bracket not open yet".to_owned(),
        },
        BlockReason::NoPermittedFreeSetup => "no free setup in this bracket's pool".to_owned(),
        BlockReason::PlayerBusy { key, source } => format!("{} busy: {}", name_for_key(state, key), busy_source_line(source)),
        BlockReason::PlayerResting { key } => format!("{} resting (d cycles flags)", name_for_key(state, key)),
        BlockReason::PlayerDeparted { key } => format!("{} departed for the night", name_for_key(state, key)),
        BlockReason::RestWindow { key, until } => {
            format!("{} inside the rest window until {}", name_for_key(state, key), fmt_clock(*until))
        }
        BlockReason::PlayerDisqualified { key } => format!("{} disqualified on site", name_for_key(state, key)),
        BlockReason::Snoozed { until } => format!("snoozed until {}", fmt_clock(*until)),
    }
}

/// Which evidence marks a player busy, with the blocking set named.
fn busy_source_line(source: &BusySource) -> String {
    match source {
        BusySource::LocalSetup { setup, bracket, set } => {
            format!("on setup {} ({} R{} {})", setup.0, short_name(bracket), set.round, set.identifier)
        }
        BusySource::RemoteActive { bracket, set } => {
            format!("started remotely in {} (R{} {})", short_name(bracket), set.round, set.identifier)
        }
        BusySource::RemoteCalled { bracket, set } => {
            format!("called remotely in {} (R{} {})", short_name(bracket), set.round, set.identifier)
        }
        BusySource::SoftDeviation { bracket, set } => {
            format!(
                "unrecognized state change in {} (R{} {})",
                short_name(bracket),
                set.round,
                set.identifier
            )
        }
    }
}

/// Best-effort display name for a conflict key (scans current snapshots).
fn name_for_key(state: &AppState, key: &ConflictKey) -> String {
    state
        .brackets
        .iter()
        .flat_map(|b| b.state.sets.iter())
        .flat_map(|s| s.occupants())
        .find(|o| occupant_keys(o, &state.aliases).contains(key))
        .map(|o| o.display_name.clone())
        .unwrap_or_else(|| match key {
            ConflictKey::Player(p) => format!("player {}", p.0),
            ConflictKey::Entrant(e) => format!("entrant {}", e.0),
        })
}

fn draw_notices(frame: &mut Frame<'_>, state: &AppState, selected: usize, now: UnixMillis) {
    let area = centered_rect(frame.area(), 80, 70);
    frame.render_widget(Clear, area);

    let selected = selected.min(state.notices.len().saturating_sub(1));
    let header = Row::new(["age", "level", "", "notice"]).style(Style::new().add_modifier(Modifier::BOLD));
    let rows = state.notices.iter().rev().enumerate().map(|(ix, notice)| {
        let (level, style) = match notice.level {
            NoticeLevel::Info => ("info", Style::new().fg(Color::Gray)),
            NoticeLevel::Warn => ("warn", Style::new().fg(Color::Yellow)),
            NoticeLevel::Error => ("ERROR", Style::new().fg(Color::Red)),
        };
        let ack = if notice.acked { "✓" } else { "·" };
        let row = Row::new([fmt_age(now - notice.at), level.to_owned(), ack.to_owned(), notice.text.clone()]).style(style);
        if ix == selected {
            row.style(SELECTED)
        } else {
            row
        }
    });
    let widths = [
        Constraint::Length(7),
        Constraint::Length(6),
        Constraint::Length(2),
        Constraint::Min(40),
    ];
    let unread = state.notices.iter().filter(|n| !n.acked && n.level != NoticeLevel::Info).count();
    let title = format!("Notices ({unread} unread) — Enter acks, c clears all, Esc closes");
    render_with_selection(
        frame,
        area,
        Table::new(rows, widths).header(header).block(Block::bordered().title(title)),
        selected,
        &state.ui.modal_view,
    );
}

fn draw_pending_writes(frame: &mut Frame<'_>, state: &AppState, selected: usize) {
    let area = centered_rect(frame.area(), 80, 70);
    frame.render_widget(Clear, area);
    let [writes_area, ledger_area] = Layout::vertical([Constraint::Min(5), Constraint::Length(7)]).areas(area);

    let selected = selected.min(state.pending_writes.len().saturating_sub(1));
    let header = Row::new(["write", "set", "bracket", "status", "tries", "last error"]).style(Style::new().add_modifier(Modifier::BOLD));
    let rows = state.pending_writes.iter().enumerate().map(|(ix, pending)| {
        let status = match pending.status {
            PendingStatus::Queued => "queued",
            PendingStatus::AwaitingReconnect => "awaiting reconnect",
            PendingStatus::Parked => "PARKED",
        };
        let row = Row::new([
            pending.intent.kind.label().to_owned(),
            pending.intent.id.to_string(),
            short_name(&pending.intent.bracket).to_owned(),
            status.to_owned(),
            pending.attempts.to_string(),
            pending.last_error.clone().map(|e| truncate(&e, 40)).unwrap_or_default(),
        ]);
        if ix == selected {
            row.style(SELECTED)
        } else {
            row
        }
    });
    let widths = [
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Length(16),
        Constraint::Length(18),
        Constraint::Length(5),
        Constraint::Min(20),
    ];
    let title = format!(
        "Pending writes ({}) — Enter retries parked, d discards, Esc closes",
        state.pending_writes.len()
    );
    render_with_selection(
        frame,
        writes_area,
        Table::new(rows, widths).header(header).block(Block::bordered().title(title)),
        selected,
        &state.ui.modal_view,
    );

    // The divergence ledger: sets the desk re-queued that the site still
    // shows CALLED — the co-TO handover reconciliation script.
    let divergent: Vec<Line<'_>> = divergence_ledger(state)
        .into_iter()
        .map(|(bracket, key)| {
            Line::from(format!(
                "{} R{} {} — {} (desk board is authoritative)",
                short_name(&bracket),
                key.round,
                key.identifier,
                players_for(state, &bracket, &key),
            ))
        })
        .collect();
    let ledger_title = format!("Remote shows called; locally re-queued ({})", divergent.len());
    frame.render_widget(
        Paragraph::new(divergent)
            .wrap(Wrap { trim: true })
            .block(Block::bordered().title(ledger_title)),
        ledger_area,
    );
}

/// Re-queued sets whose remote state still carries CALLED evidence.
fn divergence_ledger(state: &AppState) -> Vec<(BracketId, SetKey)> {
    let mut pairs: Vec<(BracketId, SetKey)> = state
        .tombstones
        .suppress_remote_called
        .iter()
        .filter(|(bracket, key)| {
            state
                .brackets
                .iter()
                .find(|b| &b.state.id == bracket)
                .and_then(|b| b.state.sets.iter().find(|s| &s.key == key))
                .is_some_and(|s| !s.is_completed() && s.called_evidence(&state.called_ints))
        })
        .cloned()
        .collect();
    pairs.sort();
    pairs
}

fn draw_reassign(frame: &mut Frame<'_>, state: &AppState, setup: crate::config::SetupId, selected: usize) {
    let area = centered_rect(frame.area(), 50, 40);
    frame.render_widget(Clear, area);

    let options = reassign_options(state);
    let rows = options.iter().enumerate().map(|(ix, option)| {
        let label = match option {
            ReassignOption::Dedicate(bracket) => format!("only {}", short_name(bracket)),
            ReassignOption::AllowAny => "allow any bracket".to_owned(),
            ReassignOption::RestoreConfig => "restore config pools".to_owned(),
        };
        let row = Row::new([label]);
        if ix == selected {
            row.style(SELECTED)
        } else {
            row
        }
    });
    let title = format!("Reassign setup {} — Enter applies, Esc cancels", setup.0);
    render_with_selection(
        frame,
        area,
        Table::new(rows, [Constraint::Min(20)]).block(Block::bordered().title(title)),
        selected,
        &state.ui.modal_view,
    );
}

fn draw_setups(frame: &mut Frame<'_>, state: &AppState, selected: usize) {
    let area = centered_rect(frame.area(), 50, 60);
    frame.render_widget(Clear, area);

    let rows = setups_rows(state).into_iter().enumerate().map(|(ix, option)| {
        let label = match option {
            SetupsRow::Retire(id, setup_type) => {
                let status = state
                    .board
                    .setups()
                    .iter()
                    .find(|s| s.id == id)
                    .map(|s| if s.status == SetupStatus::Free { "free" } else { "occupied" })
                    .unwrap_or("?");
                format!("setup {} ({setup_type}) — {status}", id.0)
            }
            SetupsRow::Add(setup_type) => format!("+ add a {setup_type} station"),
        };
        let row = Row::new([label]);
        if ix == selected {
            row.style(SELECTED)
        } else {
            row
        }
    });
    let title = if state.ui.setups_count_entry.is_empty() {
        "Stations — Enter retires (free only) / adds · digits set a count · Esc closes".to_owned()
    } else {
        let target_type = match setups_rows(state).into_iter().nth(selected) {
            Some(SetupsRow::Retire(_, ty) | SetupsRow::Add(ty)) => ty,
            None => "?".to_owned(),
        };
        format!(
            "Stations — set {target_type} to {}_ stations (Enter applies)",
            state.ui.setups_count_entry
        )
    };
    render_with_selection(
        frame,
        area,
        Table::new(rows, [Constraint::Min(20)]).block(Block::bordered().title(title)),
        selected,
        &state.ui.modal_view,
    );
}

fn draw_player_flags(frame: &mut Frame<'_>, state: &AppState, players: &[(ConflictKey, String)], selected: usize) {
    let area = centered_rect(frame.area(), 50, 40);
    frame.render_widget(Clear, area);

    let header = Row::new(["player", "flag"]).style(Style::new().add_modifier(Modifier::BOLD));
    let rows = players.iter().enumerate().map(|(ix, (key, name))| {
        let row = Row::new([name.clone(), flag_label(&state.flags, key).to_owned()]);
        if ix == selected {
            row.style(SELECTED)
        } else {
            row
        }
    });
    let widths = [Constraint::Min(20), Constraint::Length(16)];
    let title = "Player flags — Enter cycles rest/depart/force-avail, Esc closes";
    render_with_selection(
        frame,
        area,
        Table::new(rows, widths).header(header).block(Block::bordered().title(title)),
        selected,
        &state.ui.modal_view,
    );
}

fn draw_find_set(frame: &mut Frame<'_>, state: &AppState, query: &str, selected: usize) {
    let area = centered_rect(frame.area(), 60, 60);
    frame.render_widget(Clear, area);

    let rows = find_set_rows(state, query).into_iter().enumerate().map(|(ix, row)| {
        let cells = [
            format!("{}", row.setup.0),
            row.status.to_owned(),
            short_name(&row.bracket).to_owned(),
            row.round_text,
            row.players,
        ];
        let row = Row::new(cells);
        if ix == selected {
            row.style(SELECTED)
        } else {
            row
        }
    });
    let title = format!("Find set — filter: {query}_ (Enter reports, Esc closes)");
    render_with_selection(
        frame,
        area,
        Table::new(
            rows,
            [
                Constraint::Length(3),
                Constraint::Length(7),
                Constraint::Length(12),
                Constraint::Length(5),
                Constraint::Min(20),
            ],
        )
        .block(Block::bordered().title(title)),
        selected,
        &state.ui.modal_view,
    );
}

fn draw_report(frame: &mut Frame<'_>, state: &AppState, draft: &ReportDraft) {
    let area = centered_rect(frame.area(), 60, 60);
    frame.render_widget(Clear, area);

    let best_of = draft.best_of.map(|n| format!(" (Bo{n})")).unwrap_or_default();
    let title = format!(
        "Report — {} vs {}{best_of}",
        show_tag(state, &draft.left.name),
        show_tag(state, &draft.right.name)
    );

    let mut lines: Vec<Line<'_>> = Vec::new();
    lines.push(Line::from(format!(
        "score: {} {} — {} {}",
        show_tag(state, &draft.left.name),
        draft.wins(Side::Left),
        draft.wins(Side::Right),
        show_tag(state, &draft.right.name)
    )));
    for (ix, game) in draft.games.iter().enumerate() {
        let chars = match (game.chars[0], game.chars[1]) {
            (None, None) => String::new(),
            _ => format!(
                "  ·  {} vs {}",
                character_name(state, draft, &game.chars, Side::Left),
                character_name(state, draft, &game.chars, Side::Right)
            ),
        };
        let line = Line::from(format!(
            "game {}: {}{}",
            ix + 1,
            show_tag(state, &draft.side(game.winner).name),
            chars
        ));
        // The cursor marks which game `c` re-characters (that game onward).
        let targeted = matches!(draft.stage, ReportStage::Games) && ix == draft.game_cursor;
        lines.push(if targeted { line.style(SELECTED) } else { line });
    }
    if draft.games.is_empty() {
        lines.push(Line::from(format!(
            "characters: {} / {}",
            character_name(state, draft, &draft.chars, Side::Left),
            character_name(state, draft, &draft.chars, Side::Right)
        )));
    }
    lines.push(Line::from(""));

    match &draft.stage {
        ReportStage::Games => {
            lines.push(Line::from(format!(
                "1 = {} won · 2 = {} won",
                show_tag(state, &draft.left.name),
                show_tag(state, &draft.right.name)
            )));
            let chars_hint = if draft.games.len() > 1 {
                format!("c characters (game {}+, Up/Down aim)", draft.game_cursor + 1)
            } else {
                "c characters".to_owned()
            };
            lines.push(Line::from(format!(
                "{chars_hint} · d DQ · Backspace undo game · Enter finish · Esc cancel"
            )));
        }
        ReportStage::Characters { side, filter, cursor } => {
            let target = if draft.games.is_empty() {
                String::new()
            } else {
                format!(" (game {}+)", draft.game_cursor + 1)
            };
            lines.push(Line::from(format!("character for {}{target}: {filter}_", draft.side(*side).name)));
            let matches = filtered_roster(report_roster(state, &draft.bracket), filter);
            for (ix, character) in matches.iter().take(8).enumerate() {
                let line = Line::from(format!("  {}", character.name));
                lines.push(if ix == *cursor { line.style(SELECTED) } else { line });
            }
            if matches.is_empty() {
                lines.push(Line::from("  (no match)"));
            }
            lines.push(Line::from("type to filter · Enter pick · Tab keep · Esc back"));
        }
        ReportStage::DqPick => {
            lines.push(Line::from(format!(
                "DQ which side? 1 = {} · 2 = {}",
                draft.left.name, draft.right.name
            )));
            lines.push(Line::from("Esc back"));
        }
        ReportStage::Confirm { dq } => {
            lines.push(Line::from(format!("submit: {}?", draft.summary(*dq))).style(Style::new().add_modifier(Modifier::BOLD)));
            if let Some(warning) = draft.tally_warning(*dq) {
                lines.push(Line::from(format!("⚠ {warning} — submit anyway?")).style(Style::new().fg(Color::Yellow)));
            }
            lines.push(Line::from("y/Enter submit · Esc back (add more games)"));
        }
    }

    let body = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(Block::bordered().title(title));
    frame.render_widget(body, area);
}

/// The display name of one side's pick in a `[left, right]` pair.
fn character_name(state: &AppState, draft: &ReportDraft, chars: &[Option<i32>; 2], side: Side) -> String {
    let Some(id) = chars[side.ix()] else {
        return "—".to_owned();
    };
    report_roster(state, &draft.bracket)
        .iter()
        .find(|c| c.id == id)
        .map(|c| c.name.clone())
        .unwrap_or_else(|| format!("#{id}"))
}

fn draw_help(frame: &mut Frame<'_>) {
    let area = centered_rect(frame.area(), 60, 60);
    frame.render_widget(Clear, area);
    let text = [
        "1-9/0     pick a free setup (call picker) / select an occupied one",
        "          (boards past 10 stations buffer digits: 1 4 = setup 14)",
        "Enter     call the highlighted queue entry on its first free setup",
        "          (in picker: commit the selected call)",
        "p         selected setup: called -> in progress",
        "f         selected setup: free, awaiting remote result",
        "r         selected setup: un-call, set returns to the queue",
        "g         report the selected setup's set (games + characters + DQ)",
        "/         find an on-station set by player name (Enter reports it)",
        "t         toggle sponsor prefixes on player names",
        "z         snooze the highlighted queue entry (5m)",
        "d         player flags for the highlighted entry (rest/depart)",
        "a         reassign the selected setup's pool (redeploy)",
        "s         stations: add/retire setups mid-event",
        "i         inspect blocked sets (why not callable)",
        "n         notices page (Enter acks, c clears all)",
        "w         pending writes + divergence ledger",
        "Up/Down   move the queue highlight (PgUp/PgDn jump 10)",
        "u         undo the last local action (single level)",
        "q/Ctrl-C  quit",
    ]
    .into_iter()
    .map(Line::from)
    .collect::<Vec<_>>();
    let help = Paragraph::new(text).block(Block::bordered().title("Keys — Esc closes"));
    frame.render_widget(help, area);
}

/// Sponsor-prefix-aware display of one tag (`t` toggle).
fn show_tag<'a>(state: &AppState, name: &'a str) -> &'a str {
    if state.hide_sponsors {
        strip_sponsor(name)
    } else {
        name
    }
}

/// The toggle applied to an already-joined "A vs B" string.
fn show_players(state: &AppState, joined: &str) -> String {
    if !state.hide_sponsors {
        return joined.to_owned();
    }
    joined.split(" vs ").map(strip_sponsor).collect::<Vec<_>>().join(" vs ")
}

fn players_for(state: &AppState, bracket: &BracketId, key: &crate::model::SetKey) -> String {
    state
        .brackets
        .iter()
        .find(|b| &b.state.id == bracket)
        .and_then(|b| b.state.sets.iter().find(|s| &s.key == key))
        .map(|set| {
            set.occupants()
                .map(|o| truncate(show_tag(state, &o.display_name), 10))
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
        PollHealth::RateLimited => "429".to_owned(),
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
        config::{BracketConfig, BracketMode, SchedulerConfig, SetupCounts, DEFAULT_SETUP_TYPE},
        model::BracketId,
        synth::{make_se_bracket, materialize_ids},
    };

    const NOW: i64 = 1_751_000_000_000;

    fn test_state(writes_armed: bool) -> AppState {
        let mut bracket = make_se_bracket(1001, 4);
        bracket.sets = materialize_ids(&bracket.sets, 9000);
        let slug = "tournament/fbr/event/ultimate-singles";
        let config = SchedulerConfig {
            setups: Some(SetupCounts::Uniform(2)),
            brackets: vec![BracketConfig::new(slug)],
            ..SchedulerConfig::default()
        };
        let boots = vec![BracketBootstrap {
            id: BracketId(slug.to_owned()),
            sets: bracket.sets.clone(),
            groups: vec![bracket.info.clone()],
            mode: BracketMode::Full,
            start_at: None,
            setup_types: vec![DEFAULT_SETUP_TYPE.to_owned()],
            duration_prior_secs: 480,
            prior_weight: 4.0,
            characters: Vec::new(),
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
    fn report_modal_renders_stages() {
        let mut state = test_state(true);
        state.brackets[0].characters = vec![
            bracket_tools_startgg::CharacterInfo {
                id: 1,
                name: "Mario".to_owned(),
            },
            bracket_tools_startgg::CharacterInfo {
                id: 3,
                name: "Fox".to_owned(),
            },
        ];
        update(&mut state, Msg::Key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE)), NOW);
        update(&mut state, Msg::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), NOW);
        update(&mut state, Msg::Key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE)), NOW);

        let text = render(&state);
        assert!(text.contains("Report —"), "report title:\n{text}");
        assert!(text.contains("score:"), "score line:\n{text}");

        // The character picker stage shows the filtered roster.
        update(&mut state, Msg::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE)), NOW);
        update(&mut state, Msg::Key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE)), NOW);
        let text = render(&state);
        assert!(text.contains("character for"), "picker prompt:\n{text}");
        assert!(text.contains("Fox"), "filtered roster:\n{text}");
    }

    #[test]
    fn call_picker_modal_renders_candidates() {
        let mut state = test_state(false);
        update(&mut state, Msg::Key(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE)), NOW);

        let text = render(&state);
        assert!(text.contains("Call on setup 2"), "picker title:\n{text}");
        assert!(text.contains("Enter commits"), "picker hint:\n{text}");
        assert!(text.contains("greedy (rollout pending)"), "greedy fallback labeled:\n{text}");
    }

    #[test]
    fn call_picker_labels_rollout_and_renders_hold() {
        let mut state = test_state(false);
        let per_setup = state
            .world
            .per_setup
            .iter()
            .map(|(setup, entries)| {
                let mut rows: Vec<crate::world::RolloutRow> = entries
                    .iter()
                    .cloned()
                    .map(|e| crate::world::RolloutRow::Call(Box::new(e)))
                    .collect();
                rows.push(crate::world::RolloutRow::Hold {
                    waiting_for: None,
                    projected_finish: Some(NOW + 3_600_000),
                });
                (*setup, rows)
            })
            .collect();
        update(
            &mut state,
            Msg::SimResult(crate::world::RolloutRankings {
                per_setup,
                computed_at: NOW,
            }),
            NOW,
        );
        update(&mut state, Msg::Key(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE)), NOW);

        let text = render(&state);
        assert!(text.contains("[rollout"), "rollout-labeled title:\n{text}");
        assert!(text.contains("HOLD"), "hold row renders:\n{text}");
        assert!(text.contains("proj"), "projection column:\n{text}");
    }

    #[test]
    fn inspection_view_renders_block_reasons() {
        let mut state = test_state(false);
        // Call R1 A: the final stays blocked on slots; the other R1 set on
        // player-busy checks isn't (players are disjoint in an SE4).
        update(&mut state, Msg::Key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE)), NOW);
        update(&mut state, Msg::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), NOW);
        update(&mut state, Msg::Key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE)), NOW);

        let text = render(&state);
        assert!(text.contains("Blocked sets"), "inspection title:\n{text}");
        assert!(text.contains("waiting on prerequisite"), "slots reason line:\n{text}");
    }

    #[test]
    fn notices_page_renders_with_unread_count() {
        let mut state = test_state(false);
        state.notice(NOW, crate::app::NoticeLevel::Warn, "wifi looked shaky");
        update(&mut state, Msg::Key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE)), NOW);

        let text = render(&state);
        assert!(text.contains("Notices (1 unread)"), "notices title:\n{text}");
        assert!(text.contains("wifi looked shaky"), "notice body:\n{text}");
    }

    #[test]
    fn pending_writes_view_renders_divergence_ledger() {
        let mut state = test_state(true);
        // Call on setup 1, then no-show re-queue: suppress_remote_called set.
        update(&mut state, Msg::Key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE)), NOW);
        update(&mut state, Msg::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), NOW);
        let (bracket, set_key) = {
            let pending = &state.pending_writes[0].intent;
            (pending.bracket.clone(), pending.key.clone())
        };
        update(
            &mut state,
            Msg::Key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE)),
            NOW + 1000,
        );

        // The site still shows the set CALLED (state int 6).
        let ix = state.brackets.iter().position(|b| b.state.id == bracket).unwrap();
        let set = state.brackets[ix].state.sets.iter_mut().find(|s| s.key == set_key).unwrap();
        set.state_int = Some(6);
        state.called_ints = vec![6];

        update(
            &mut state,
            Msg::Key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE)),
            NOW + 2000,
        );
        let text = render(&state);
        assert!(text.contains("Pending writes"), "writes title:\n{text}");
        assert!(
            text.contains("Remote shows called; locally re-queued (1)"),
            "divergence ledger:\n{text}"
        );
    }

    #[test]
    fn summaries_show_duration_introspection() {
        let state = test_state(false);
        let text = render(&state);
        assert!(text.contains("est set"), "introspection column:\n{text}");
        assert!(text.contains("8m00s ·0"), "pure-prior estimate with zero samples:\n{text}");
    }

    #[test]
    fn help_overlay_renders() {
        let mut state = test_state(false);
        update(&mut state, Msg::Key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE)), NOW);

        let text = render(&state);
        assert!(text.contains("snooze the highlighted queue entry"), "help body:\n{text}");
    }

    #[test]
    fn setups_modal_scrolls_the_add_row_into_view() {
        // A tall board: the add row sits at the very bottom of the list; the
        // modal must scroll it into view once the cursor reaches it.
        let mut state = test_state(false);
        for id in 3..=30 {
            state.board.add_setup(crate::config::SetupId(id), "default".to_owned());
        }
        update(&mut state, Msg::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE)), NOW);
        for _ in 0..30 {
            update(&mut state, Msg::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)), NOW);
        }

        let text = render(&state);
        assert!(text.contains("+ add a default station"), "add row visible:\n{text}");

        // Edge-scrolling: stepping back inside the view must not move the
        // viewport (a fresh offset each frame would re-pin the cursor to the
        // bottom edge).
        let offset = state.ui.modal_view.scroll.get();
        assert!(offset > 0, "the long list scrolled");
        update(&mut state, Msg::Key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)), NOW);
        render(&state);
        assert_eq!(
            state.ui.modal_view.scroll.get(),
            offset,
            "offset holds while the cursor is inside the view"
        );
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

//! The desk's dialogs over `ui.modal`: the call picker, the report flow,
//! the stations editor, the set finder, and the browse/fix dialogs the
//! terminal shell keeps behind letter keys.

use bracket_tools_scheduler_core::{
    app::{
        blocked_entries, divergence_ledger, filtered_roster, find_set_rows, flag_label, picker_rows, reassign_options, report_roster,
        setups_rows, AppState, Modal, NoticeLevel, PendingStatus, ReassignOption, ReportDraft, ReportStage, SetupsRow,
    },
    config::SetupId,
    conflict::{ConflictKey, PoolOverride, SetupStatus},
    keymap::HELP_LINES,
    model::{BracketId, SetKey},
    text::{fmt_age, players_line, reason_line, reason_tag, short_name},
    timers::now_millis,
    ui_action::{ReportAction, Side, UiAction},
    world::RolloutRow,
};
use dioxus::prelude::*;

use crate::{
    bridge::Session,
    views::{dispatcher, level_class},
};

/// Whichever dialog the state has open.
#[component]
pub fn Modals(session: Session) -> Element {
    let modal = session.state.read().ui.modal.clone();
    match modal {
        Some(Modal::CallPicker { setup, selected, .. }) => rsx! { CallPicker { session, setup, selected } },
        Some(Modal::Report(draft)) => rsx! { Report { session, draft: *draft } },
        Some(Modal::Setups { selected }) => rsx! { Setups { session, selected } },
        Some(Modal::FindSet { query, selected }) => rsx! { FindSet { session, query, selected } },
        Some(Modal::Reassign { setup, selected }) => rsx! { Reassign { session, setup, selected } },
        Some(Modal::PlayerFlags { players, selected }) => rsx! { Flags { session, players, selected } },
        Some(Modal::PendingWrites { selected }) => rsx! { PendingWrites { session, selected } },
        Some(Modal::Inspection { selected }) => rsx! { Inspection { session, selected } },
        Some(Modal::Notices { selected }) => rsx! { Notices { session, selected } },
        Some(Modal::Help) => rsx! { Help { session } },
        None => rsx! {},
    }
}

#[component]
fn Dialog(title: String, #[props(default)] hint: String, children: Element) -> Element {
    rsx! {
        div { class: "modal",
            div { class: "dialog",
                h2 {
                    {title}
                    if !hint.is_empty() {
                        span { class: "hint", {hint} }
                    }
                }
                {children}
            }
        }
    }
}

/// The keyboard cursor's row class.
fn targeted(is_cursor: bool) -> &'static str {
    if is_cursor {
        "targeted"
    } else {
        ""
    }
}

#[component]
fn CallPicker(session: Session, setup: SetupId, selected: usize) -> Element {
    let state = session.state.read();
    let (rows, from_rollout) = picker_rows(&state, setup);
    let empty = rows.is_empty();
    rsx! {
        Dialog {
            title: format!("Setup {}", setup.0),
            hint: if from_rollout { "rollout ranking" } else { "greedy ranking" },
            ul {
                for (ix, row) in rows.into_iter().enumerate() {
                    match row {
                        RolloutRow::Call(entry) => rsx! {
                            li { class: targeted(ix == selected),
                                span { class: "label", "{entry.players} — {entry.round_text} ({entry.bracket.0})" }
                                button {
                                    onclick: dispatcher(&session, UiAction::CallSet { setup, bracket: entry.bracket.clone(), key: entry.key.clone() }),
                                    "call"
                                }
                            }
                        },
                        RolloutRow::Hold { .. } => rsx! { li { class: "hold {targeted(ix == selected)}", "hold this setup open" } },
                    }
                }
                if empty {
                    li { class: "hold", "nothing callable on this setup's pool" }
                }
            }
            div { class: "row",
                if empty {
                    button { class: "quiet", onclick: dispatcher(&session, UiAction::OpenReassign(setup)), "reassign pool" }
                }
                button { class: "quiet", onclick: dispatcher(&session, UiAction::CloseModal), "close" }
            }
        }
    }
}

#[component]
fn Report(session: Session, draft: ReportDraft) -> Element {
    let state = session.state.read();
    let best_of = draft.best_of.map(|n| format!(" (Bo{n})")).unwrap_or_default();
    let in_games = draft.stage == ReportStage::Games;
    rsx! {
        Dialog { title: "Report — {draft.left.name} vs {draft.right.name}{best_of}",
            p { class: "score",
                "{draft.left.name} "
                strong { "{draft.wins(Side::Left)} – {draft.wins(Side::Right)}" }
                " {draft.right.name}"
            }
            ul { class: "games",
                for (ix, game) in draft.games.iter().enumerate() {
                    li {
                        class: targeted(in_games && ix == draft.game_cursor),
                        onclick: dispatcher(&session, report(ReportAction::TargetGame(ix))),
                        span { class: "label", "game {ix + 1}: {draft.side(game.winner).name}" }
                        if game.chars.iter().any(Option::is_some) {
                            span { class: "hint", "{character_names(&state, &draft, &game.chars)}" }
                        }
                    }
                }
                if draft.games.is_empty() {
                    li { class: "hold", "characters: {character_names(&state, &draft, &draft.chars)}" }
                }
            }
            match &draft.stage {
                ReportStage::Games => games_stage(&session, &state, &draft),
                ReportStage::Characters { side, filter, cursor } => characters_stage(&session, &state, &draft, *side, filter, *cursor),
                ReportStage::DqPick => dq_stage(&session, &draft),
                ReportStage::Confirm { dq } => confirm_stage(&session, &draft, *dq),
            }
        }
    }
}

fn games_stage(session: &Session, state: &AppState, draft: &ReportDraft) -> Element {
    let has_roster = !report_roster(state, &draft.bracket).is_empty();
    let can_finish = !draft.games.is_empty() && draft.leader().is_some();
    let characters = if draft.games.len() > 1 {
        format!("characters (game {}+)", draft.game_cursor + 1)
    } else {
        "characters".to_owned()
    };
    rsx! {
        div { class: "row",
            button { onclick: dispatcher(session, report(ReportAction::RecordGame(Side::Left))), "{draft.left.name} won" }
            button { onclick: dispatcher(session, report(ReportAction::RecordGame(Side::Right))), "{draft.right.name} won" }
        }
        div { class: "row",
            button {
                class: "quiet",
                disabled: !has_roster,
                title: if has_roster { None } else { Some("no character data for this event") },
                onclick: dispatcher(session, report(ReportAction::OpenCharacterPicker)),
                {characters}
            }
            button { class: "quiet", disabled: draft.games.is_empty(), onclick: dispatcher(session, report(ReportAction::UndoGame)), "undo game" }
            button { class: "quiet", onclick: dispatcher(session, report(ReportAction::StartDq)), "DQ" }
            button {
                disabled: !can_finish,
                title: if can_finish { None } else { Some("record game winners until one side leads") },
                onclick: dispatcher(session, report(ReportAction::FinishGames)),
                "finish"
            }
            button { class: "quiet", onclick: dispatcher(session, UiAction::CloseModal), "cancel" }
        }
    }
}

fn characters_stage(session: &Session, state: &AppState, draft: &ReportDraft, side: Side, filter: &str, cursor: usize) -> Element {
    let matches = filtered_roster(report_roster(state, &draft.bracket), filter);
    let target = if draft.games.is_empty() {
        String::new()
    } else {
        format!(" (game {}+)", draft.game_cursor + 1)
    };
    // Enter takes the cursor's match; with none it keeps the current pick,
    // as the keymap does.
    let on_enter = matches.get(cursor).map(|c| c.id);
    let typed = session.clone();
    let keyed = session.clone();
    rsx! {
        p { "character for {draft.side(side).name}{target}" }
        input {
            r#type: "text",
            value: filter,
            autofocus: true,
            placeholder: "type to filter",
            oninput: move |event| typed.dispatch(report(ReportAction::SetCharacterFilter(event.value()))),
            onkeydown: move |event| match event.key() {
                Key::Enter => keyed.dispatch(report(ReportAction::PickCharacter(on_enter))),
                Key::Escape => keyed.dispatch(report(ReportAction::Back)),
                _ => {}
            },
        }
        ul { class: "roster",
            for (ix, character) in matches.iter().enumerate() {
                li { class: targeted(ix == cursor),
                    span { class: "label", "{character.name}" }
                    button { onclick: dispatcher(session, report(ReportAction::PickCharacter(Some(character.id)))), "pick" }
                }
            }
            if matches.is_empty() {
                li { class: "hold", "no match" }
            }
        }
        div { class: "row",
            button { class: "quiet", onclick: dispatcher(session, report(ReportAction::PickCharacter(None))), "keep current" }
            button { class: "quiet", onclick: dispatcher(session, report(ReportAction::Back)), "back" }
        }
    }
}

fn dq_stage(session: &Session, draft: &ReportDraft) -> Element {
    rsx! {
        p { "DQ which side?" }
        div { class: "row",
            button { onclick: dispatcher(session, report(ReportAction::PickDq(Side::Left))), "{draft.left.name}" }
            button { onclick: dispatcher(session, report(ReportAction::PickDq(Side::Right))), "{draft.right.name}" }
            button { class: "quiet", onclick: dispatcher(session, report(ReportAction::Back)), "back" }
        }
    }
}

fn confirm_stage(session: &Session, draft: &ReportDraft, dq: Option<Side>) -> Element {
    rsx! {
        p { class: "confirm", "submit: {draft.summary(dq)}?" }
        if let Some(warning) = draft.tally_warning(dq) {
            p { class: "warn", "⚠ {warning} — submit anyway?" }
        }
        div { class: "row",
            button { onclick: dispatcher(session, report(ReportAction::Submit)), "submit" }
            button { class: "quiet", onclick: dispatcher(session, report(ReportAction::Back)), "back (add more games)" }
        }
    }
}

fn report(action: ReportAction) -> UiAction {
    UiAction::Report(action)
}

/// "Mario / Fox" for a `[left, right]` pick pair.
fn character_names(state: &AppState, draft: &ReportDraft, chars: &[Option<i32>; 2]) -> String {
    let roster = report_roster(state, &draft.bracket);
    let name = |pick: Option<i32>| match pick {
        Some(id) => roster
            .iter()
            .find(|c| c.id == id)
            .map_or_else(|| format!("#{id}"), |c| c.name.clone()),
        None => "—".to_owned(),
    };
    format!("{} / {}", name(chars[0]), name(chars[1]))
}

#[component]
fn Setups(session: Session, selected: usize) -> Element {
    let state = session.state.read();
    let count_of = |setup_type: &str| state.board.setups().iter().filter(|s| s.setup_type == setup_type).count();
    rsx! {
        Dialog { title: "Stations", hint: "retire free stations, add arrivals, or set a type's count",
            ul {
                for (ix, row) in setups_rows(&state).into_iter().enumerate() {
                    match row {
                        SetupsRow::Retire(id, setup_type) => {
                            let free = state.board.setups().iter().any(|s| s.id == id && s.status == SetupStatus::Free);
                            rsx! {
                                li { class: targeted(ix == selected),
                                    span { class: "label",
                                        "setup {id.0} "
                                        span { class: "hint", "{setup_type} · " if free { "free" } else { "occupied" } }
                                    }
                                    button {
                                        class: "quiet",
                                        disabled: !free,
                                        title: if free { None } else { Some("occupied — free it first") },
                                        onclick: dispatcher(&session, UiAction::RetireSetup(id)),
                                        "retire"
                                    }
                                }
                            }
                        }
                        SetupsRow::Add(setup_type) => rsx! {
                            li { class: "add {targeted(ix == selected)}",
                                span { class: "label", "{count_of(&setup_type)} × {setup_type}" }
                                TypeCount { key: "{setup_type}", session: session.clone(), setup_type: setup_type.clone() }
                                button { onclick: dispatcher(&session, UiAction::AddSetup(setup_type.clone())), "add one" }
                            }
                        },
                    }
                }
            }
            button { class: "quiet", onclick: dispatcher(&session, UiAction::CloseModal), "close" }
        }
    }
}

/// A typed target count for one setup type, applied by the button or Enter.
#[component]
fn TypeCount(session: Session, setup_type: String) -> Element {
    let mut entry = use_signal(String::new);
    let target = entry().trim().parse::<u32>().ok();
    let apply = use_callback(move |()| {
        if let Ok(target) = entry().trim().parse::<u32>() {
            session.dispatch(UiAction::SetSetupCount {
                setup_type: setup_type.clone(),
                target,
            });
            entry.set(String::new());
        }
    });
    rsx! {
        input {
            r#type: "number",
            min: "0",
            placeholder: "count",
            value: entry(),
            oninput: move |event| entry.set(event.value()),
            onkeydown: move |event| if event.key() == Key::Enter { apply(()) },
        }
        button { class: "quiet", disabled: target.is_none(), onclick: move |_| apply(()), "set count" }
    }
}

#[component]
fn FindSet(session: Session, query: String, selected: usize) -> Element {
    let state = session.state.read();
    let rows = find_set_rows(&state, &query);
    let armed = state.writes_armed;
    let none_match = rows.is_empty();
    let first = rows.first().map(|row| row.setup);
    let typed = session.clone();
    let keyed = session.clone();
    rsx! {
        Dialog { title: "Find set", hint: "on-station sets by player name",
            input {
                r#type: "text",
                value: query,
                autofocus: true,
                placeholder: "player name",
                oninput: move |event| typed.dispatch(UiAction::SetFindQuery(event.value())),
                onkeydown: move |event| match event.key() {
                    Key::Enter => {
                        if let Some(setup) = first {
                            report_found(&keyed, setup);
                        }
                    }
                    Key::Escape => keyed.dispatch(UiAction::CloseModal),
                    _ => {}
                },
            }
            table {
                thead {
                    tr { th { "setup" } th { "status" } th { "bracket" } th { "round" } th { "players" } th {} }
                }
                tbody {
                    for (ix, row) in rows.into_iter().enumerate() {
                        tr { class: targeted(ix == selected),
                            td { "{row.setup.0}" }
                            td { "{row.status}" }
                            td { {short_name(&row.bracket)} }
                            td { "{row.round_text}" }
                            td { "{row.players}" }
                            td {
                                button {
                                    disabled: !armed,
                                    title: if armed { None } else { Some("reporting needs writes armed") },
                                    onclick: {
                                        let session = session.clone();
                                        move |_| report_found(&session, row.setup)
                                    },
                                    "report"
                                }
                            }
                        }
                    }
                }
            }
            if none_match {
                p { class: "hint", "no on-station set matches" }
            }
            button { class: "quiet", onclick: dispatcher(&session, UiAction::CloseModal), "close" }
        }
    }
}

/// Opens the report for a found set. The finder closes first so a refusal's
/// notice lands on the desk instead of behind the dialog.
fn report_found(session: &Session, setup: SetupId) {
    session.dispatch(UiAction::CloseModal);
    session.dispatch(UiAction::OpenReport(setup));
}

#[component]
fn Reassign(session: Session, setup: SetupId, selected: usize) -> Element {
    let state = session.state.read();
    let current = match state.pool_overrides.get(&setup) {
        Some(PoolOverride::Dedicated(bracket)) => format!("only {}", short_name(bracket)),
        Some(PoolOverride::AllowAny) => "any bracket".to_owned(),
        None => "config pools".to_owned(),
    };
    rsx! {
        Dialog { title: "Reassign setup {setup.0}", hint: "now: {current}",
            ul {
                for (ix, option) in reassign_options(&state).into_iter().enumerate() {
                    li { class: targeted(ix == selected),
                        span { class: "label", {reassign_label(&option)} }
                        button { onclick: dispatcher(&session, UiAction::ReassignSetup { setup, option: option.clone() }), "apply" }
                    }
                }
            }
            button { class: "quiet", onclick: dispatcher(&session, UiAction::CloseModal), "close" }
        }
    }
}

fn reassign_label(option: &ReassignOption) -> String {
    match option {
        ReassignOption::Dedicate(bracket) => format!("only {}", short_name(bracket)),
        ReassignOption::AllowAny => "allow any bracket".to_owned(),
        ReassignOption::RestoreConfig => "restore config pools".to_owned(),
    }
}

#[component]
fn Flags(session: Session, players: Vec<(ConflictKey, String)>, selected: usize) -> Element {
    let state = session.state.read();
    rsx! {
        Dialog { title: "Player flags", hint: "resting → departed → force-available → clear",
            ul {
                for (ix, (key, name)) in players.iter().enumerate() {
                    li { class: targeted(ix == selected),
                        span { class: "label",
                            "{name} "
                            span { class: "hint", {flag_label(&state.flags, key)} }
                        }
                        button { onclick: dispatcher(&session, UiAction::CycleFlagFor(key.clone())), "cycle" }
                    }
                }
            }
            button { class: "quiet", onclick: dispatcher(&session, UiAction::CloseModal), "close" }
        }
    }
}

#[component]
fn PendingWrites(session: Session, selected: usize) -> Element {
    let state = session.state.read();
    let ledger = divergence_ledger(&state);
    rsx! {
        Dialog { title: "Pending writes ({state.pending_writes.len()})", hint: "parked writes wait for a retry or a discard",
            table {
                thead {
                    tr { th { "write" } th { "set" } th { "bracket" } th { "status" } th { class: "num", "tries" } th { "last error" } th {} }
                }
                tbody {
                    for (ix, pending) in state.pending_writes.iter().enumerate() {
                        tr { class: targeted(ix == selected),
                            td { {pending.intent.kind.label()} }
                            td { "{pending.intent.id}" }
                            td { {short_name(&pending.intent.bracket)} }
                            td { {pending_status(pending.status)} }
                            td { class: "num", "{pending.attempts}" }
                            td { class: "hint", {pending.last_error.clone().unwrap_or_default()} }
                            td {
                                if pending.status == PendingStatus::Parked {
                                    button { onclick: dispatcher(&session, UiAction::RetryWrite(Box::new(pending.intent.clone()))), "retry" }
                                    " "
                                    button { class: "quiet", onclick: dispatcher(&session, UiAction::DiscardWrite(Box::new(pending.intent.clone()))), "discard" }
                                }
                            }
                        }
                    }
                }
            }
            if state.pending_writes.is_empty() {
                p { class: "hint", "nothing pending" }
            }
            h3 { "Remote shows called, locally re-queued ({ledger.len()})" }
            ul {
                for (bracket, key) in ledger.iter() {
                    li { {ledger_line(&state, bracket, key)} }
                }
            }
            button { class: "quiet", onclick: dispatcher(&session, UiAction::CloseModal), "close" }
        }
    }
}

fn pending_status(status: PendingStatus) -> &'static str {
    match status {
        PendingStatus::Queued => "queued",
        PendingStatus::AwaitingReconnect => "awaiting reconnect",
        PendingStatus::Parked => "PARKED",
    }
}

fn ledger_line(state: &AppState, bracket: &BracketId, key: &SetKey) -> String {
    format!(
        "{} R{} {} — {} (desk board is authoritative)",
        short_name(bracket),
        key.round,
        key.identifier,
        players_line(state, bracket, key)
    )
}

#[component]
fn Inspection(session: Session, selected: usize) -> Element {
    let state = session.state.read();
    let entries = blocked_entries(&state);
    rsx! {
        Dialog { title: "Blocked sets ({entries.len()})", hint: "why a set isn't callable; open a row for the details",
            ul { class: "blocked",
                for (ix, (bracket, key)) in entries.iter().enumerate() {
                    li { class: targeted(ix == selected),
                        details { open: ix == selected,
                            summary {
                                strong { {short_name(bracket)} }
                                " R{key.round} {key.identifier} — {players_line(&state, bracket, key)} "
                                span { class: "hint", {reason_tags(&state, bracket, key)} }
                            }
                            ul {
                                for reason in state.world.blocked.get(&(bracket.clone(), key.clone())).into_iter().flatten() {
                                    li { {reason_line(&state, reason)} }
                                }
                            }
                        }
                    }
                }
            }
            if entries.is_empty() {
                p { class: "hint", "nothing is blocked" }
            }
            button { class: "quiet", onclick: dispatcher(&session, UiAction::CloseModal), "close" }
        }
    }
}

fn reason_tags(state: &AppState, bracket: &BracketId, key: &SetKey) -> String {
    state
        .world
        .blocked
        .get(&(bracket.clone(), key.clone()))
        .map(|reasons| reasons.iter().map(reason_tag).collect::<Vec<_>>().join(", "))
        .unwrap_or_default()
}

#[component]
fn Notices(session: Session, selected: usize) -> Element {
    let state = session.state.read();
    let now = now_millis();
    let unread = state.notices.iter().filter(|n| !n.acked && n.level != NoticeLevel::Info).count();
    rsx! {
        Dialog { title: "Notices", hint: "{unread} unread",
            table {
                thead {
                    tr { th { "age" } th { "level" } th { "notice" } th {} }
                }
                tbody {
                    for (ix, notice) in state.notices.iter().rev().enumerate() {
                        tr { class: "{level_class(notice.level, notice.acked)} {targeted(ix == selected)}",
                            td { {fmt_age(now - notice.at)} }
                            td { {level_text(notice.level)} }
                            td { "{notice.text}" }
                            td {
                                if !notice.acked {
                                    button { class: "quiet", onclick: dispatcher(&session, UiAction::AckNoticeAt(notice.at)), "ok" }
                                }
                            }
                        }
                    }
                }
            }
            div { class: "row",
                button { class: "quiet", onclick: dispatcher(&session, UiAction::ClearNotices), "clear all" }
                button { class: "quiet", onclick: dispatcher(&session, UiAction::CloseModal), "close" }
            }
        }
    }
}

fn level_text(level: NoticeLevel) -> &'static str {
    match level {
        NoticeLevel::Info => "info",
        NoticeLevel::Warn => "warn",
        NoticeLevel::Error => "ERROR",
    }
}

#[component]
fn Help(session: Session) -> Element {
    rsx! {
        Dialog { title: "Keys", hint: "the desk's keyboard, the same as the terminal's",
            pre { class: "keys", {HELP_LINES.join("\n")} }
            button { class: "quiet", onclick: dispatcher(&session, UiAction::CloseModal), "close" }
        }
    }
}

//! The desk: stations, the queue, bracket summaries, notices, and the call
//! picker, all rendered from the session's `AppState`.

use bracket_tools_scheduler_core::{
    app::{picker_rows, AppState, Modal, NoticeLevel},
    config::SetupId,
    conflict::{SetupStatus, UnixMillis},
    model::{BracketId, SetKey},
    timers::now_millis,
    ui_action::UiAction,
    world::RolloutRow,
};
use dioxus::prelude::*;

use crate::bridge::Session;

#[component]
pub fn Desk(session: Session, mode: String) -> Element {
    rsx! {
        main { class: "desk",
            div { class: "toolbar",
                span { class: "mode", {mode} }
                button { class: "quiet", onclick: dispatcher(&session, UiAction::Undo), "undo" }
            }
            Stations { session: session.clone() }
            section { class: "columns",
                Queue { session: session.clone() }
                aside {
                    Summaries { session: session.clone() }
                    Notices { session: session.clone() }
                }
            }
            Picker { session: session.clone() }
        }
    }
}

#[component]
fn Stations(session: Session) -> Element {
    let state = session.state.read();
    rsx! {
        section { class: "stations",
            for setup in state.board.setups().iter().cloned() {
                div { class: "station {status_class(&setup.status)}",
                    div { class: "placard", "{setup.id.0}" span { class: "type", "{setup.setup_type}" } }
                    div { class: "occupant", "{occupant_text(&state, &setup.status)}" }
                    div { class: "actions",
                        match setup.status {
                            SetupStatus::Free => rsx! {
                                button { onclick: dispatcher(&session, UiAction::SelectSetup(setup.id)), "pick" }
                            },
                            SetupStatus::Called { .. } => rsx! {
                                button { onclick: dispatcher(&session, UiAction::Progress(setup.id)), "started" }
                                button { class: "quiet", onclick: dispatcher(&session, UiAction::Free(setup.id)), "free" }
                                button { class: "quiet", onclick: dispatcher(&session, UiAction::Requeue(setup.id)), "re-queue" }
                            },
                            SetupStatus::InProgress { .. } => rsx! {
                                button { onclick: dispatcher(&session, UiAction::Free(setup.id)), "free" }
                                button { class: "quiet", onclick: dispatcher(&session, UiAction::Requeue(setup.id)), "re-queue" }
                            },
                            SetupStatus::OccupiedExternal { .. } => rsx! { span { "in use elsewhere" } },
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn Queue(session: Session) -> Element {
    let state = session.state.read();
    rsx! {
        section { class: "queue",
            h2 { "Queue" }
            table {
                thead {
                    tr { th { "#" } th { "set" } th { "round" } th { "bracket" } th { class: "num", "score" } th { "setups" } th {} }
                }
                tbody {
                    for (ix, entry) in state.world.queue.iter().enumerate() {
                        tr {
                            td { "{ix + 1}" }
                            td { "{entry.players}" }
                            td { "{entry.round_text}" }
                            td { "{entry.bracket.0}" }
                            td { class: "num", "{entry.candidate.score:.1}" }
                            td { "{setups_text(&entry.candidate_setups)}" }
                            td {
                                button { onclick: dispatcher(&session, quick_call(&entry.bracket, &entry.key)), "call" }
                                " "
                                button { class: "quiet", onclick: dispatcher(&session, snooze(&entry.bracket, &entry.key)), "snooze" }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn Summaries(session: Session) -> Element {
    let state = session.state.read();
    let now = now_millis();
    rsx! {
        h2 { "Brackets" }
        for summary in &state.world.summaries {
            div { class: "summary",
                strong { "{summary.id.0}" }
                span { "{summary.incomplete_sets} left" }
                span { "critical path {summary.critical_path}" }
                span { "{finish_text(summary.projected_finish, summary.projection_blocked, now)}" }
            }
        }
    }
}

#[component]
fn Notices(session: Session) -> Element {
    let state = session.state.read();
    rsx! {
        h2 { "Notices" }
        for notice in state.notices.iter().rev().take(8) {
            div { class: "notice {level_class(notice.level, notice.acked)}",
                span { class: "text", "{notice.text}" }
                if !notice.acked {
                    button { class: "quiet", onclick: dispatcher(&session, UiAction::AckNoticeAt(notice.at)), "ok" }
                }
            }
        }
    }
}

#[component]
fn Picker(session: Session) -> Element {
    let state = session.state.read();
    let Some(Modal::CallPicker { setup, .. }) = state.ui.modal.clone() else {
        return rsx! {};
    };
    let (rows, from_rollout) = picker_rows(&state, setup);
    rsx! {
        div { class: "modal",
            div { class: "dialog",
                h2 {
                    "Setup {setup.0}"
                    span { class: "hint", if from_rollout { "rollout ranking" } else { "greedy ranking" } }
                }
                ul {
                    for row in rows {
                        match row {
                            RolloutRow::Call(entry) => rsx! {
                                li {
                                    span { class: "label", "{entry.players} — {entry.round_text} ({entry.bracket.0})" }
                                    button { onclick: dispatcher(&session, call_set(setup, &entry.bracket, &entry.key)), "call" }
                                }
                            },
                            RolloutRow::Hold { .. } => rsx! { li { class: "hold", "hold this setup open" } },
                        }
                    }
                }
                button { class: "quiet", onclick: dispatcher(&session, UiAction::CloseModal), "close" }
            }
        }
    }
}

/// A click handler that sends one intent.
fn dispatcher(session: &Session, action: UiAction) -> impl FnMut(Event<MouseData>) {
    let session = session.clone();
    move |_| session.dispatch(action.clone())
}

fn quick_call(bracket: &BracketId, key: &SetKey) -> UiAction {
    UiAction::QuickCallSet {
        bracket: bracket.clone(),
        key: key.clone(),
    }
}

fn snooze(bracket: &BracketId, key: &SetKey) -> UiAction {
    UiAction::SnoozeSet {
        bracket: bracket.clone(),
        key: key.clone(),
    }
}

fn call_set(setup: SetupId, bracket: &BracketId, key: &SetKey) -> UiAction {
    UiAction::CallSet {
        setup,
        bracket: bracket.clone(),
        key: key.clone(),
    }
}

fn status_class(status: &SetupStatus) -> &'static str {
    match status {
        SetupStatus::Free => "free",
        SetupStatus::Called { .. } => "called",
        SetupStatus::InProgress { .. } => "in-progress",
        SetupStatus::OccupiedExternal { .. } => "external",
    }
}

fn occupant_text(state: &AppState, status: &SetupStatus) -> String {
    let (bracket, key, verb) = match status {
        SetupStatus::Free => return "free".to_owned(),
        SetupStatus::Called { bracket, set } => (bracket, set, "called"),
        SetupStatus::InProgress { bracket, set } => (bracket, set, "playing"),
        SetupStatus::OccupiedExternal { .. } => return "occupied".to_owned(),
    };
    let players = state
        .find_set(bracket, key)
        .map(|set| set.occupants().map(|o| o.display_name.as_str()).collect::<Vec<_>>().join(" vs "))
        .unwrap_or_default();
    format!("{verb}: {players}")
}

fn setups_text(setups: &[SetupId]) -> String {
    setups.iter().map(|s| s.0.to_string()).collect::<Vec<_>>().join(" ")
}

fn finish_text(finish: Option<UnixMillis>, blocked: bool, now: UnixMillis) -> String {
    match finish {
        Some(at) => format!("~{}m to finish", ((at - now).max(0) / 60_000)),
        None if blocked => "starved".to_owned(),
        None => "no projection".to_owned(),
    }
}

fn level_class(level: NoticeLevel, acked: bool) -> String {
    let level = match level {
        NoticeLevel::Info => "info",
        NoticeLevel::Warn => "warn",
        NoticeLevel::Error => "error",
    };
    if acked {
        format!("{level} acked")
    } else {
        level.to_owned()
    }
}

//! The desk: stations, the queue, bracket summaries, notices, and the call
//! picker, all rendered from the session's `AppState`.

use bracket_tools_scheduler_core::{
    app::{AppState, NoticeLevel, PendingStatus},
    config::SetupId,
    conflict::{SetupStatus, UnixMillis},
    model::{BracketId, SetKey},
    text::players_line,
    timers::now_millis,
    ui_action::UiAction,
};
use dioxus::prelude::*;

use crate::{bridge::Session, modals::Modals, page::use_page_events};

/// The desk; `toolbar` adds shell-specific controls next to undo.
#[component]
pub fn Desk(session: Session, mode: String, #[props(default = VNode::empty())] toolbar: Element) -> Element {
    use_page_events(session.clone());
    let (writes_armed, persist_failed, blocked, pending, parked, digits) = {
        let state = session.state.read();
        (
            state.writes_armed,
            state.persist_failed,
            state.world.blocked.len(),
            state.pending_writes.len(),
            state.pending_writes.iter().filter(|w| w.status == PendingStatus::Parked).count(),
            state.ui.setup_entry.as_ref().map(|entry| entry.digits.clone()),
        )
    };
    rsx! {
        main { class: "desk",
            div { class: "toolbar",
                span { class: "mode", {mode} }
                span { class: if writes_armed { "writes armed" } else { "writes" },
                    if writes_armed { "writes armed" } else { "advisor-only" }
                }
                if persist_failed {
                    span { class: "writes armed", "saves failing" }
                }
                if let Some(digits) = digits {
                    span { class: "hint", "setup {digits}_" }
                }
                div { class: "tools",
                    {toolbar}
                    button { class: "quiet", onclick: dispatcher(&session, UiAction::OpenSetups), "stations" }
                    button { class: "quiet", onclick: dispatcher(&session, UiAction::OpenFindSet), "find set" }
                    button { class: "quiet", onclick: dispatcher(&session, UiAction::OpenInspection), "blocked {blocked}" }
                    button {
                        class: if parked > 0 { "quiet alert" } else { "quiet" },
                        onclick: dispatcher(&session, UiAction::OpenPendingWrites),
                        "writes {pending}"
                    }
                    button { class: "quiet", onclick: dispatcher(&session, UiAction::OpenHelp), "keys" }
                    button { class: "quiet", onclick: dispatcher(&session, UiAction::Undo), "undo" }
                }
            }
            Stations { session: session.clone() }
            section { class: "columns",
                Queue { session: session.clone() }
                aside {
                    Summaries { session: session.clone() }
                    Notices { session: session.clone() }
                }
            }
            Modals { session: session.clone() }
        }
    }
}

#[component]
fn Stations(session: Session) -> Element {
    let state = session.state.read();
    let report = |setup: SetupId| {
        rsx! {
            button {
                disabled: !state.writes_armed,
                title: if state.writes_armed { None } else { Some("reporting needs writes armed") },
                onclick: dispatcher(&session, UiAction::OpenReport(setup)),
                "report"
            }
        }
    };
    rsx! {
        section { class: "stations",
            for setup in state.board.setups().iter().cloned() {
                div { class: "station {status_class(&setup.status)} {selected_class(&state, setup.id)}",
                    div { class: "placard", "{setup.id.0}" span { class: "type", "{setup.setup_type}" } }
                    div { class: "occupant", "{occupant_text(&state, &setup.status)}" }
                    div { class: "actions",
                        match setup.status {
                            SetupStatus::Free => rsx! {
                                button { onclick: dispatcher(&session, UiAction::SelectSetup(setup.id)), "pick" }
                            },
                            SetupStatus::Called { .. } => rsx! {
                                button { onclick: dispatcher(&session, UiAction::Progress(setup.id)), "started" }
                                {report(setup.id)}
                                button { class: "quiet", onclick: dispatcher(&session, UiAction::Free(setup.id)), "free" }
                                button { class: "quiet", onclick: dispatcher(&session, UiAction::Requeue(setup.id)), "re-queue" }
                            },
                            SetupStatus::InProgress { .. } => rsx! {
                                {report(setup.id)}
                                button { class: "quiet", onclick: dispatcher(&session, UiAction::Free(setup.id)), "free" }
                                button { class: "quiet", onclick: dispatcher(&session, UiAction::Requeue(setup.id)), "re-queue" }
                            },
                            SetupStatus::OccupiedExternal { .. } => rsx! { span { "in use elsewhere" } },
                        }
                        button { class: "quiet", onclick: dispatcher(&session, UiAction::OpenReassign(setup.id)), "pool" }
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
                        tr { class: if ix == state.ui.queue_ix { "cursor" } else { "" },
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
                                " "
                                button { class: "quiet", onclick: dispatcher(&session, flags(&entry.bracket, &entry.key)), "flags" }
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
        h2 {
            "Notices"
            button { class: "quiet link", onclick: dispatcher(&session, UiAction::OpenNotices), "all" }
        }
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

/// A click handler that sends one intent.
pub(crate) fn dispatcher(session: &Session, action: UiAction) -> impl FnMut(Event<MouseData>) {
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

fn flags(bracket: &BracketId, key: &SetKey) -> UiAction {
    UiAction::OpenFlagsFor {
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
    format!("{verb}: {}", players_line(state, bracket, key))
}

fn selected_class(state: &AppState, setup: SetupId) -> &'static str {
    if state.ui.selected_setup == Some(setup) {
        "selected"
    } else {
        ""
    }
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

pub(crate) fn level_class(level: NoticeLevel, acked: bool) -> String {
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

//! Display text both shells share: block-reason explanations, player
//! lines, clocks and ages, bracket short names.

use chrono::{DateTime, Local};

use crate::{
    app::AppState,
    conflict::{occupant_keys, BlockReason, BusySource, ConflictKey, UnixMillis},
    model::{strip_sponsor, BracketId, SetKey},
};

/// The bracket id's last path segment.
pub fn short_name(id: &BracketId) -> &str {
    id.0.rsplit('/').next().unwrap_or(&id.0)
}

/// `name` cut to `max` characters with an ellipsis.
pub fn truncate(name: &str, max: usize) -> String {
    if name.chars().count() <= max {
        name.to_owned()
    } else {
        name.chars().take(max.saturating_sub(1)).chain(['…']).collect()
    }
}

/// "42s" / "3m05s" / "1h02m".
pub fn fmt_age(millis: i64) -> String {
    let secs = (millis / 1000).max(0);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    }
}

/// The local wall clock, HH:MM.
pub fn fmt_clock(millis: UnixMillis) -> String {
    DateTime::from_timestamp_millis(millis)
        .map(|utc| utc.with_timezone(&Local).format("%H:%M").to_string())
        .unwrap_or_else(|| "?".to_owned())
}

/// Sponsor-prefix-aware display of one tag (the `t` toggle).
pub fn show_tag<'a>(state: &AppState, name: &'a str) -> &'a str {
    if state.hide_sponsors {
        strip_sponsor(name)
    } else {
        name
    }
}

/// "A vs B" for a set on the local table, sponsor-aware; empty once the set
/// is gone.
pub fn players_line(state: &AppState, bracket: &BracketId, key: &SetKey) -> String {
    state
        .find_set(bracket, key)
        .map(|set| {
            set.occupants()
                .map(|o| show_tag(state, &o.display_name))
                .collect::<Vec<_>>()
                .join(" vs ")
        })
        .unwrap_or_default()
}

/// Short tag for the row summary column.
pub fn reason_tag(reason: &BlockReason) -> &'static str {
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
pub fn reason_line(state: &AppState, reason: &BlockReason) -> String {
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
pub fn name_for_key(state: &AppState, key: &ConflictKey) -> String {
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

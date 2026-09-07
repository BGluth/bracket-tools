//! Change detection between two sweeps. Pure: no I/O, so a failed sweep can
//! never reach it and be mistaken for "everything disappeared".

use bracket_tools_startgg::TournamentSummary;
use chrono::DateTime;

use crate::snapshot::Snapshot;

#[derive(Debug, PartialEq)]
pub enum Change {
    Appeared(TournamentSummary),
    Changed {
        after: TournamentSummary,
        fields: Vec<FieldChange>,
    },
    /// Left the sweep while still inside the history window: unpublished,
    /// deleted, or moved out of the region.
    Gone(TournamentSummary),
}

#[derive(Debug, PartialEq)]
pub struct FieldChange {
    pub field: &'static str,
    pub before: String,
    pub after: String,
}

/// Appeared and changed tournaments in start order, then the gone ones.
/// `window_start` is the sweep's `after` bound: a tournament that dropped out
/// because its start fell behind it simply aged out and is not a change.
pub fn diff(prev: &Snapshot, cur: &Snapshot, window_start: i64) -> Vec<Change> {
    let mut changes = Vec::new();
    for after in cur.by_start() {
        match prev.tournaments.get(&after.id) {
            None => changes.push(Change::Appeared(after.clone())),
            Some(before) => {
                let fields = field_changes(before, after);
                if !fields.is_empty() {
                    changes.push(Change::Changed {
                        after: after.clone(),
                        fields,
                    });
                }
            }
        }
    }
    for before in prev.by_start() {
        if !cur.tournaments.contains_key(&before.id) && !aged_out(before, window_start) {
            changes.push(Change::Gone(before.clone()));
        }
    }
    changes
}

fn aged_out(tournament: &TournamentSummary, window_start: i64) -> bool {
    tournament.start_at.is_some_and(|start| start < window_start)
}

/// The fields a consumer cares about. Attendee counts and start.gg's own
/// `updatedAt` are deliberately not tracked: both move without a real edit.
fn field_changes(before: &TournamentSummary, after: &TournamentSummary) -> Vec<FieldChange> {
    let mut changes = Vec::new();
    let mut check = |field: &'static str, b: String, a: String| {
        if b != a {
            changes.push(FieldChange {
                field,
                before: b,
                after: a,
            });
        }
    };
    check("name", text(&before.name), text(&after.name));
    check("start", time(before.start_at), time(after.start_at));
    check("end", time(before.end_at), time(after.end_at));
    check("city", text(&before.city), text(&after.city));
    check("venue", text(&before.venue_name), text(&after.venue_name));
    check("address", text(&before.venue_address), text(&after.venue_address));
    check("registration", flag(before.registration_open), flag(after.registration_open));
    check(
        "registration closes",
        time(before.registration_closes_at),
        time(after.registration_closes_at),
    );
    check("events", events(before), events(after));
    changes
}

fn text(value: &Option<String>) -> String {
    value.clone().unwrap_or_else(|| "-".to_string())
}

pub fn time(unix_secs: Option<i64>) -> String {
    unix_secs
        .and_then(|secs| DateTime::from_timestamp(secs, 0))
        .map_or_else(|| "-".to_string(), |dt| dt.format("%Y-%m-%d %H:%M UTC").to_string())
}

fn flag(open: Option<bool>) -> String {
    match open {
        Some(true) => "open",
        Some(false) => "closed",
        None => "-",
    }
    .to_string()
}

fn events(tournament: &TournamentSummary) -> String {
    let mut names: Vec<String> = tournament
        .events
        .iter()
        .map(|e| format!("{} ({})", text(&e.name), text(&e.videogame)))
        .collect();
    names.sort();
    names.join(", ")
}

#[cfg(test)]
mod tests {
    use super::{diff, Change};
    use crate::{
        snapshot::Snapshot,
        test_support::{event, summary},
    };

    const WINDOW_START: i64 = 1000;

    fn snapshot(items: Vec<bracket_tools_startgg::TournamentSummary>) -> Snapshot {
        Snapshot::from_sweep(0, items)
    }

    #[test]
    fn appeared_changed_and_gone_in_that_order() {
        let mut moved = summary(2, "b", Some(2000));
        moved.venue_name = Some("Old hall".into());
        let prev = snapshot(vec![summary(1, "a", Some(1500)), moved.clone(), summary(3, "c", Some(3000))]);

        moved.venue_name = Some("New hall".into());
        moved.start_at = Some(2100);
        let cur = snapshot(vec![summary(4, "d", Some(1200)), moved, summary(3, "c", Some(3000))]);

        let changes = diff(&prev, &cur, WINDOW_START);
        assert_eq!(changes.len(), 3);
        assert!(matches!(&changes[0], Change::Appeared(t) if t.id == 4));
        match &changes[1] {
            Change::Changed { after, fields, .. } => {
                assert_eq!(after.id, 2);
                let names: Vec<&str> = fields.iter().map(|f| f.field).collect();
                assert_eq!(names, vec!["start", "venue"]);
                assert_eq!(fields[1].before, "Old hall");
                assert_eq!(fields[1].after, "New hall");
            }
            other => panic!("expected Changed, got {other:?}"),
        }
        assert!(matches!(&changes[2], Change::Gone(t) if t.id == 1));
    }

    #[test]
    fn noise_fields_do_not_count_as_changes() {
        let mut before = summary(1, "a", Some(2000));
        before.num_attendees = Some(10);
        before.updated_at = Some(5);
        let mut after = before.clone();
        after.num_attendees = Some(14);
        after.updated_at = Some(9);

        assert!(diff(&snapshot(vec![before]), &snapshot(vec![after]), WINDOW_START).is_empty());
    }

    #[test]
    fn event_list_changes_are_order_insensitive() {
        let mut before = summary(1, "a", Some(2000));
        before.events = vec![event(1, "Singles", "Ultimate"), event(2, "Melee", "Melee")];
        let mut same = before.clone();
        same.events.reverse();
        let mut added = before.clone();
        added.events.push(event(3, "Doubles", "Ultimate"));

        assert!(diff(&snapshot(vec![before.clone()]), &snapshot(vec![same]), WINDOW_START).is_empty());
        let changes = diff(&snapshot(vec![before]), &snapshot(vec![added]), WINDOW_START);
        assert!(matches!(&changes[0], Change::Changed { fields, .. } if fields[0].field == "events"));
    }

    #[test]
    fn aging_out_of_the_window_is_silent_but_vanishing_inside_it_is_gone() {
        let prev = snapshot(vec![
            summary(1, "old", Some(WINDOW_START - 1)),
            summary(2, "recent", Some(WINDOW_START + 1)),
            summary(3, "undated", None),
        ]);
        let cur = snapshot(vec![]);

        let gone: Vec<u64> = diff(&prev, &cur, WINDOW_START)
            .into_iter()
            .filter_map(|c| match c {
                Change::Gone(t) => Some(t.id),
                _ => None,
            })
            .collect();

        assert_eq!(gone, vec![2, 3]);
    }
}

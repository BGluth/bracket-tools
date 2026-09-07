//! The iCalendar feed: one VEVENT per tournament with a stable UID, so a
//! calendar subscribed to the file's URL sees edits as edits and removals as
//! removals. Times are UTC; the subscriber renders them in its own zone.

use bracket_tools_startgg::TournamentSummary;
use chrono::{DateTime, Duration, Utc};
use icalendar::{Calendar, Component, Event, EventLike};

use crate::{
    config::{series_name, Series},
    snapshot::Snapshot,
};

const UID_DOMAIN: &str = "gg-watch";
const DEFAULT_DURATION_HOURS: i64 = 4;

pub fn render(snapshot: &Snapshot, series: &[Series], calendar_name: &str, generated_at: i64) -> String {
    let stamp = utc(generated_at).unwrap_or_else(Utc::now);
    let mut calendar = Calendar::new();
    calendar.name(calendar_name);
    for tournament in snapshot.by_start() {
        if let Some(event) = event(tournament, series, stamp) {
            calendar.push(event);
        }
    }
    calendar.done().to_string()
}

/// Undated tournaments have no place on a calendar and are skipped.
fn event(t: &TournamentSummary, series: &[Series], stamp: DateTime<Utc>) -> Option<Event> {
    let start = utc(t.start_at?)?;
    let end = t
        .end_at
        .and_then(utc)
        .filter(|end| *end > start)
        .unwrap_or(start + Duration::hours(DEFAULT_DURATION_HOURS));

    let mut event = Event::new();
    event
        .uid(&format!("startgg-{}@{UID_DOMAIN}", t.id))
        .summary(t.name.as_deref().unwrap_or(&t.slug))
        .starts(start)
        .ends(end)
        .url(&t.url())
        .description(&description(t, series))
        .timestamp(stamp);
    if let Some(location) = location(t) {
        event.location(&location);
    }
    if let Some(modified) = t.updated_at.and_then(utc) {
        event.last_modified(modified);
    }

    Some(event.done())
}

/// Venue, address, city — skipping the city when the address already names it.
fn location(t: &TournamentSummary) -> Option<String> {
    let address = t.venue_address.as_deref();
    let city = t
        .city
        .as_deref()
        .filter(|city| !address.is_some_and(|a| a.to_lowercase().contains(&city.to_lowercase())));
    let parts: Vec<&str> = [t.venue_name.as_deref(), address, city].into_iter().flatten().collect();
    (!parts.is_empty()).then(|| parts.join(", "))
}

fn description(t: &TournamentSummary, series: &[Series]) -> String {
    let mut lines = Vec::new();
    if let Some(name) = series_name(series, t) {
        lines.push(format!("Series: {name}"));
    }
    let games = t.games();
    if !games.is_empty() {
        lines.push(format!("Games: {}", games.join(", ")));
    }
    lines.push(match t.registration_open {
        Some(true) => "Registration: open".to_string(),
        Some(false) => "Registration: closed".to_string(),
        None => "Registration: unknown".to_string(),
    });
    lines.push(t.url());
    lines.join("\n")
}

fn utc(unix_secs: i64) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp(unix_secs, 0)
}

#[cfg(test)]
mod tests {
    use super::render;
    use crate::{
        snapshot::Snapshot,
        test_support::{event, summary},
    };

    #[test]
    fn one_vevent_per_dated_tournament_with_stable_uids() {
        let mut dated = summary(7, "weekly-7", Some(1_700_000_000));
        dated.name = Some("Weekly #7".into());
        dated.city = Some("Town".into());
        dated.venue_name = Some("Hall".into());
        dated.events = vec![event(1, "Singles", "Ultimate")];
        let with_end = {
            let mut t = summary(8, "big-1", Some(1_700_000_000));
            t.end_at = Some(1_700_090_000);
            t.venue_address = Some("1 Main St, Town, AB".into());
            t.city = Some("Town".into());
            t
        };
        let snapshot = Snapshot::from_sweep(0, vec![dated, with_end, summary(9, "undated", None)]);

        let ics = render(&snapshot, &[], "Test calendar", 1_700_000_000);

        assert_eq!(ics.matches("BEGIN:VEVENT").count(), 2);
        assert!(ics.contains("X-WR-CALNAME:Test calendar"));
        assert!(ics.contains("UID:startgg-7@gg-watch"));
        assert!(ics.contains("SUMMARY:Weekly #7"));
        assert!(ics.contains("DTSTART:20231114T221320Z"));
        // No end on start.gg: a default block. With one: as given.
        assert!(ics.contains("DTEND:20231115T021320Z"));
        assert!(ics.contains("DTEND:20231115T231320Z"));
        assert!(ics.contains("LOCATION:Hall\\, Town"));
        // The city is not repeated when the address already names it.
        assert!(ics.contains("LOCATION:1 Main St\\, Town\\, AB\r\n"));
        assert!(ics.contains("Games: Ultimate"));
        assert!(ics.contains("URL:https://www.start.gg/tournament/weekly-7"));
    }
}

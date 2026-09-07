//! Fixture builders shared by the unit tests.

use bracket_tools_startgg::{EventSummary, StartGgId, TournamentSummary};

pub fn summary(id: StartGgId, bare_slug: &str, start_at: Option<i64>) -> TournamentSummary {
    TournamentSummary {
        id,
        slug: format!("tournament/{bare_slug}"),
        name: Some(format!("T {id}")),
        start_at,
        end_at: None,
        created_at: None,
        updated_at: None,
        city: None,
        addr_state: None,
        country_code: None,
        venue_name: None,
        venue_address: None,
        timezone: None,
        registration_open: None,
        registration_closes_at: None,
        num_attendees: None,
        owner_id: None,
        events: Vec::new(),
    }
}

pub fn event(id: StartGgId, name: &str, videogame: &str) -> EventSummary {
    EventSummary {
        id,
        name: Some(name.to_string()),
        videogame: Some(videogame.to_string()),
    }
}

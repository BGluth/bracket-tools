//! The JSON feed: the sweep window as a flat list in start order, for a
//! website or script to consume without knowing start.gg's shapes.

use anyhow::Result;
use bracket_tools_startgg::{StartGgId, TournamentSummary};
use serde::Serialize;

use crate::{
    config::{series_name, Series},
    snapshot::Snapshot,
};

#[derive(Debug, Serialize)]
pub struct Feed<'a> {
    pub generated_at: i64,
    pub tournaments: Vec<Entry<'a>>,
}

#[derive(Debug, Serialize)]
pub struct Entry<'a> {
    pub id: StartGgId,
    pub slug: &'a str,
    pub url: String,
    pub name: Option<&'a str>,
    pub series: Option<&'a str>,
    pub start_at: Option<i64>,
    pub end_at: Option<i64>,
    pub timezone: Option<&'a str>,
    pub city: Option<&'a str>,
    pub venue_name: Option<&'a str>,
    pub venue_address: Option<&'a str>,
    pub registration_open: Option<bool>,
    pub registration_closes_at: Option<i64>,
    pub num_attendees: Option<i32>,
    pub games: Vec<&'a str>,
    pub events: Vec<EventEntry<'a>>,
}

#[derive(Debug, Serialize)]
pub struct EventEntry<'a> {
    pub id: StartGgId,
    pub name: Option<&'a str>,
    pub videogame: Option<&'a str>,
}

pub fn build<'a>(snapshot: &'a Snapshot, series: &'a [Series], generated_at: i64) -> Feed<'a> {
    Feed {
        generated_at,
        tournaments: snapshot.by_start().into_iter().map(|t| entry(t, series)).collect(),
    }
}

pub fn render(feed: &Feed) -> Result<String> {
    Ok(serde_json::to_string_pretty(feed)?)
}

fn entry<'a>(t: &'a TournamentSummary, series: &'a [Series]) -> Entry<'a> {
    Entry {
        id: t.id,
        slug: &t.slug,
        url: t.url(),
        name: t.name.as_deref(),
        series: series_name(series, t),
        start_at: t.start_at,
        end_at: t.end_at,
        timezone: t.timezone.as_deref(),
        city: t.city.as_deref(),
        venue_name: t.venue_name.as_deref(),
        venue_address: t.venue_address.as_deref(),
        registration_open: t.registration_open,
        registration_closes_at: t.registration_closes_at,
        num_attendees: t.num_attendees,
        games: t.games(),
        events: t
            .events
            .iter()
            .map(|e| EventEntry {
                id: e.id,
                name: e.name.as_deref(),
                videogame: e.videogame.as_deref(),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::{build, render};
    use crate::{
        config::Series,
        snapshot::Snapshot,
        test_support::{event, summary},
    };

    #[test]
    fn renders_in_start_order_with_series_labels() {
        let mut weekly = summary(2, "weekly-9", Some(200));
        weekly.owner_id = Some(42);
        weekly.events = vec![event(1, "Singles", "Ultimate"), event(2, "Doubles", "Ultimate")];
        let snapshot = Snapshot::from_sweep(0, vec![weekly, summary(1, "other-1", Some(100))]);
        let series = vec![Series {
            name: "Weekly".into(),
            owner_id: Some(42),
            stems: vec!["weekly".into()],
        }];

        let json: Value = serde_json::from_str(&render(&build(&snapshot, &series, 7)).unwrap()).unwrap();
        let list = json["tournaments"].as_array().unwrap();

        assert_eq!(json["generated_at"], 7);
        assert_eq!(list[0]["id"], 1);
        assert_eq!(list[0]["series"], Value::Null);
        assert_eq!(list[1]["series"], "Weekly");
        assert_eq!(list[1]["url"], "https://www.start.gg/tournament/weekly-9");
        assert_eq!(list[1]["games"], serde_json::json!(["Ultimate"]));
        assert_eq!(list[1]["events"].as_array().unwrap().len(), 2);
    }
}

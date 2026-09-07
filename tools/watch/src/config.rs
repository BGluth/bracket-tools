//! `watch.toml`: the region to sweep, the series names to label tournaments
//! with, and where the feeds go. Lives in the XDG config dir alongside the
//! other bracket-tools configs; the snapshot and default feed paths live in
//! the XDG data dir.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use bracket_tools_startgg::{
    slug::{bare_slug, series_stem},
    LocationRadius, StartGgId, TournamentFilter, TournamentSummary,
};
use directories::ProjectDirs;
use serde::Deserialize;

use crate::files::expand_home;

const CONFIG_FILE: &str = "watch.toml";
const STATE_DIR: &str = "watch";
const SNAPSHOT_FILE: &str = "snapshot.json";
const JSON_FEED_FILE: &str = "tournaments.json";
const ICS_FEED_FILE: &str = "tournaments.ics";
const DEFAULT_HISTORY_DAYS: u32 = 14;
const DEFAULT_CALENDAR_NAME: &str = "Tournaments";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// start.gg token file; the CLI flag and `$STARTGG_TOKEN` take precedence.
    pub token_file: Option<PathBuf>,
    /// How long a finished tournament stays in the sweep (and the feeds).
    #[serde(default = "default_history_days")]
    pub history_days: u32,
    pub region: Region,
    #[serde(default)]
    pub series: Vec<Series>,
    #[serde(default)]
    pub feeds: Feeds,
}

/// The sweep area. At least one of `country`, `state`, or `radius` is
/// required: an unbounded sweep would list every tournament on start.gg.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Region {
    /// ISO 3166-1 alpha-2, e.g. `"CA"`.
    pub country: Option<String>,
    /// Province / state code as start.gg stores it, e.g. `"AB"`.
    pub state: Option<String>,
    pub radius: Option<Radius>,
    /// Only tournaments with an event in one of these games (start.gg ids);
    /// empty means every game.
    #[serde(default)]
    pub videogame_ids: Vec<StartGgId>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Radius {
    pub lat: f64,
    pub lng: f64,
    /// start.gg's notation: `"50mi"`, `"100km"`.
    pub distance: String,
}

/// A named series: a tournament belongs to it when the owner matches (if
/// set) and its slug stem is one of `stems` (if any are set).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Series {
    pub name: String,
    pub owner_id: Option<StartGgId>,
    #[serde(default)]
    pub stems: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Feeds {
    pub json: Option<PathBuf>,
    pub ics: Option<PathBuf>,
    /// The calendar's display name (`X-WR-CALNAME`).
    pub calendar_name: Option<String>,
}

fn default_history_days() -> u32 {
    DEFAULT_HISTORY_DAYS
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path).with_context(|| format!("reading config {}", path.display()))?;
        let config: Self = toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
        config.validate()?;

        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        let region = &self.region;
        if region.country.is_none() && region.state.is_none() && region.radius.is_none() {
            bail!("[region] needs at least one of country, state, or radius (an unbounded sweep would list every tournament on start.gg)");
        }
        for series in &self.series {
            if series.owner_id.is_none() && series.stems.is_empty() {
                bail!("series `{}` needs an owner_id or at least one slug stem", series.name);
            }
        }

        Ok(())
    }

    /// The sweep: the configured region, published tournaments only,
    /// starting at or after `after`.
    pub fn filter(&self, after: i64) -> TournamentFilter {
        TournamentFilter {
            country_code: self.region.country.clone(),
            addr_state: self.region.state.clone(),
            radius: self.region.radius.as_ref().map(|r| LocationRadius {
                lat: r.lat,
                lng: r.lng,
                distance: r.distance.clone(),
            }),
            after: Some(after),
            published: Some(true),
            videogame_ids: self.region.videogame_ids.clone(),
            ..TournamentFilter::default()
        }
    }

    pub fn snapshot_path(&self) -> PathBuf {
        state_dir().join(SNAPSHOT_FILE)
    }

    pub fn json_path(&self) -> PathBuf {
        self.feeds
            .json
            .as_deref()
            .map_or_else(|| state_dir().join(JSON_FEED_FILE), expand_home)
    }

    pub fn ics_path(&self) -> PathBuf {
        self.feeds
            .ics
            .as_deref()
            .map_or_else(|| state_dir().join(ICS_FEED_FILE), expand_home)
    }

    pub fn calendar_name(&self) -> &str {
        self.feeds.calendar_name.as_deref().unwrap_or(DEFAULT_CALENDAR_NAME)
    }
}

impl Series {
    pub fn matches(&self, tournament: &TournamentSummary) -> bool {
        let owner_ok = self.owner_id.is_none_or(|owner| tournament.owner_id == Some(owner));
        let stem_ok = self.stems.is_empty() || {
            let bare = bare_slug(&tournament.slug);
            let stem = series_stem(&bare);
            self.stems.iter().any(|s| s == stem)
        };
        owner_ok && stem_ok
    }
}

/// The first configured series the tournament belongs to.
pub fn series_name<'a>(series: &'a [Series], tournament: &TournamentSummary) -> Option<&'a str> {
    series.iter().find(|s| s.matches(tournament)).map(|s| s.name.as_str())
}

pub fn default_config_path() -> PathBuf {
    project_dirs().map_or_else(|| PathBuf::from(CONFIG_FILE), |dirs| dirs.config_dir().join(CONFIG_FILE))
}

fn state_dir() -> PathBuf {
    project_dirs().map_or_else(|| PathBuf::from(STATE_DIR), |dirs| dirs.data_dir().join(STATE_DIR))
}

fn project_dirs() -> Option<ProjectDirs> {
    ProjectDirs::from("", "", "bracket-tools")
}

#[cfg(test)]
mod tests {
    use super::{series_name, Config};
    use crate::test_support::summary;

    const SAMPLE: &str = r#"
history_days = 7

[region]
country = "CA"
state = "AB"
videogame_ids = [1, 1386]

[[series]]
name = "Weekly"
owner_id = 42
stems = ["weekly", "the-weekly"]

[[series]]
name = "Anything by 77"
owner_id = 77

[[series]]
name = "Monthly"
stems = ["monthly"]

[feeds]
calendar_name = "Local tournaments"
"#;

    #[test]
    fn parses_and_builds_the_sweep_filter() {
        let config: Config = toml::from_str(SAMPLE).unwrap();
        config.validate().unwrap();
        let filter = config.filter(1000);

        assert_eq!(filter.addr_state.as_deref(), Some("AB"));
        assert_eq!(filter.after, Some(1000));
        assert_eq!(filter.published, Some(true));
        assert_eq!(filter.videogame_ids, vec![1, 1386]);
        assert_eq!(config.history_days, 7);
        assert_eq!(config.calendar_name(), "Local tournaments");
    }

    #[test]
    fn rejects_an_unbounded_region_and_an_empty_series() {
        let unbounded: Config = toml::from_str("[region]\n").unwrap();
        assert!(unbounded.validate().is_err());

        let empty_series: Config = toml::from_str("[region]\ncountry = \"CA\"\n[[series]]\nname = \"x\"\n").unwrap();
        assert!(empty_series.validate().is_err());
    }

    #[test]
    fn series_matching_uses_owner_and_stem() {
        let config: Config = toml::from_str(SAMPLE).unwrap();
        let label = |slug: &str, owner: Option<u64>| {
            let mut t = summary(1, slug, None);
            t.owner_id = owner;
            series_name(&config.series, &t)
        };

        assert_eq!(label("weekly-12", Some(42)), Some("Weekly"));
        assert_eq!(label("the-weekly-3", Some(42)), Some("Weekly"));
        assert_eq!(label("weekly-12", Some(9)), None);
        assert_eq!(label("random-thing", Some(77)), Some("Anything by 77"));
        assert_eq!(label("monthly-4", Some(9)), Some("Monthly"));
        assert_eq!(label("monthly", None), Some("Monthly"));
        assert_eq!(label("other-1", None), None);
    }
}

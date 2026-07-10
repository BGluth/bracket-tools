//! `--init-tournament`: one command from a start.gg tournament URL to a
//! ready-to-review `scheduler.toml`.
//!
//! The generated config carries only what is tournament-specific: the event
//! list (with each event's setup types, looked up from the operator's global
//! game mapping), the identity pin, and per-tournament state-file paths.
//! Station counts deliberately stay OUT of it — the 's' modal owns those at
//! runtime and persists them to the cross-tournament defaults file.

use std::{collections::BTreeMap, fmt::Write as _, fs, path::Path};

use bracket_tools_startgg::EventInfo;
use serde::Deserialize;
use thiserror::Error;

/// The operator's global game mapping, in the XDG config dir next to
/// `scheduler.toml`.
pub const GAME_SETUPS_FILE: &str = "game-setups.toml";

/// Written when no mapping exists yet; parses as an empty mapping until the
/// operator uncomments entries.
pub const GAME_SETUPS_TEMPLATE: &str = r#"# game-setups.toml — maps start.gg videogames to the setup types their
# events run on. `--init-tournament` reads this to seed each generated
# event's `setup_type`.
#
# A table's key matches case-insensitively as a SUBSTRING of the start.gg
# videogame name, so [game.melee] matches "Super Smash Bros. Melee". Setup
# type names are your own labels; station counts per type live in
# setup-defaults.toml and the in-tool 's' modal, not here.
#
# The optional minutes seed each event's duration prior (used until live
# completions teach the model): prior = setup_minutes + 2.5 * game_minutes.
#
# [game.melee]
# setup_types = ["crt"]
# game_minutes = 5
# setup_minutes = 2
#
# [game.ultimate]
# setup_types = ["switch"]
# game_minutes = 7
# setup_minutes = 2
"#;

/// A bo3 set averages about 2.5 games; the duration model is bo3-normalized,
/// so seeded priors use the same basis.
const BO3_AVERAGE_GAMES: f64 = 2.5;

#[derive(Debug, Error)]
pub enum InitError {
    #[error("cannot read a tournament slug out of {input:?} (expected a start.gg URL or a `tournament/<slug>`)")]
    BadSlug { input: String },

    #[error("tournament {slug:?} answered no events (slug typo, or a hidden/unpublished tournament?)")]
    NoEvents { slug: String },
}

/// The parsed `game-setups.toml`.
#[derive(Debug, Default, Deserialize)]
pub struct GameSetups {
    #[serde(default)]
    pub game: BTreeMap<String, GameEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GameEntry {
    #[serde(default)]
    pub setup_types: Vec<String>,
    /// Average minutes one game takes (seeds the duration prior).
    pub game_minutes: Option<f64>,
    /// Fixed per-set overhead: controllers, tags, character select.
    pub setup_minutes: Option<f64>,
}

impl GameEntry {
    /// The seeded bo3-normalized duration prior, when minutes are given.
    pub fn prior_secs(&self) -> Option<u64> {
        let game = self.game_minutes?;
        let setup = self.setup_minutes.unwrap_or(0.0);
        Some(((setup + BO3_AVERAGE_GAMES * game) * 60.0).round() as u64)
    }
}

impl GameSetups {
    /// Best-effort load: a missing or unparseable file is an empty mapping
    /// (init still generates a config; events just get the default pool).
    pub fn load(path: &Path) -> Self {
        fs::read_to_string(path)
            .ok()
            .and_then(|raw| toml::from_str(&raw).ok())
            .unwrap_or_default()
    }

    /// The entry whose key appears (case-insensitively) inside the start.gg
    /// videogame name; the longest key wins so "melee hd" beats "melee".
    pub fn match_game(&self, videogame: &str) -> Option<&GameEntry> {
        let name = videogame.to_lowercase();
        self.game
            .iter()
            .filter(|(key, _)| name.contains(&key.to_lowercase()))
            .max_by_key(|(key, _)| key.len())
            .map(|(_, entry)| entry)
    }
}

/// Accepts a full start.gg URL, `tournament/<slug>`, or a bare slug, and
/// yields the pinned `tournament/<slug>` form.
pub fn parse_tournament_slug(input: &str) -> Result<String, InitError> {
    let bad = || InitError::BadSlug { input: input.to_owned() };
    let trimmed = input.trim().trim_start_matches('/');
    let slug = match trimmed.split_once("tournament/") {
        Some((_, rest)) => rest.split('/').next().unwrap_or(""),
        // A bare slug — but reject anything URL-shaped that lacks the
        // tournament segment ("start.gg/foo" is probably a typo'd URL).
        None if trimmed.contains("://") || trimmed.contains('.') && trimmed.contains('/') => return Err(bad()),
        None => trimmed,
    };
    if slug.is_empty() || slug.contains(['?', '#']) {
        return Err(bad());
    }
    Ok(format!("tournament/{slug}"))
}

/// Renders the generated config. `data_dir` anchors the per-tournament state
/// files so two tournaments never share crash-recovery state.
pub fn generate_config(tournament_slug: &str, events: &[EventInfo], setups: &GameSetups, data_dir: &Path) -> String {
    let bare = tournament_slug.strip_prefix("tournament/").unwrap_or(tournament_slug);
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# scheduler.toml — generated by `scheduler --init-tournament {tournament_slug}` ({} events).\n\
         # Review before running: delete events you are not calling, adjust setup\n\
         # types, then run `scheduler` in this directory.\n\
         #\n\
         # Station counts are deliberately NOT set here: the tool assumes your saved\n\
         # per-type defaults (setup-defaults.toml) and you adjust live with the 's'\n\
         # modal, which persists the new counts for next time.\n",
        events.len()
    );
    let _ = writeln!(out, "tournament_slug = \"{tournament_slug}\"\n");
    let _ = writeln!(
        out,
        "# Chaotic-event option: calls mark sets started immediately (no\n\
         # called/waiting phase, no no-show alerts).\n\
         # call_action = \"in_progress\"\n"
    );
    let _ = write!(
        out,
        "# Per-tournament state (crash recovery + offline cold-start cache).\n\
         state_file = \"{}\"\n\
         snapshot_file = \"{}\"",
        data_dir.join(format!("{bare}-state.json")).display(),
        data_dir.join(format!("{bare}-snapshot.json")).display(),
    );

    for event in events {
        let _ = writeln!(out, "\n[[brackets]]");
        let title = match (&event.name, &event.videogame) {
            (Some(name), Some(game)) => format!("# {name} — {game}"),
            (Some(name), None) => format!("# {name}"),
            (None, Some(game)) => format!("# {game}"),
            (None, None) => String::new(),
        };
        if !title.is_empty() {
            let _ = writeln!(out, "{title}");
        }
        let _ = writeln!(out, "slug = \"{}\"", event.slug);
        match event.videogame.as_deref().and_then(|game| setups.match_game(game)) {
            Some(entry) => {
                match entry.setup_types.as_slice() {
                    [] => {}
                    [only] => {
                        let _ = writeln!(out, "setup_type = \"{only}\"");
                    }
                    many => {
                        let list = many.iter().map(|t| format!("\"{t}\"")).collect::<Vec<_>>().join(", ");
                        let _ = writeln!(out, "setup_type = [{list}]");
                    }
                }
                if let Some(prior) = entry.prior_secs() {
                    let _ = writeln!(out, "duration_prior_secs = {prior} # seeded from game-setups.toml");
                }
            }
            None => {
                let _ = writeln!(out, "# no game-setups.toml entry matched — this event uses the shared default pool");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use bracket_tools_startgg::EventInfo;

    use super::{generate_config, parse_tournament_slug, GameSetups, InitError};
    use crate::config::{OneOrMany, SchedulerConfig};

    fn event(slug: &str, name: &str, game: Option<&str>) -> EventInfo {
        EventInfo {
            slug: slug.to_owned(),
            name: Some(name.to_owned()),
            videogame: game.map(str::to_owned),
        }
    }

    fn mapping() -> GameSetups {
        toml::from_str(
            r#"
            [game.melee]
            setup_types = ["crt"]
            game_minutes = 5
            setup_minutes = 2

            [game.ultimate]
            setup_types = ["switch", "pokemon"]
            "#,
        )
        .unwrap()
    }

    #[test]
    fn slug_parsing_accepts_urls_and_bare_forms() {
        for input in [
            "https://www.start.gg/tournament/french-bread-rumble-100/events",
            "start.gg/tournament/french-bread-rumble-100",
            "tournament/french-bread-rumble-100",
            "french-bread-rumble-100",
        ] {
            assert_eq!(
                parse_tournament_slug(input).unwrap(),
                "tournament/french-bread-rumble-100",
                "{input}"
            );
        }
        for bad in ["", "https://www.start.gg/", "start.gg/user/foo"] {
            assert!(matches!(parse_tournament_slug(bad), Err(InitError::BadSlug { .. })), "{bad}");
        }
    }

    #[test]
    fn matching_is_case_insensitive_substring_longest_wins() {
        let setups = mapping();
        let melee = setups.match_game("Super Smash Bros. Melee").unwrap();
        assert_eq!(melee.setup_types, vec!["crt"]);
        assert_eq!(melee.prior_secs(), Some(870), "2 + 2.5*5 minutes");
        assert!(setups.match_game("Rivals of Aether II").is_none());
    }

    #[test]
    fn generated_config_parses_validates_and_carries_the_mapping() {
        let events = vec![
            event(
                "tournament/fbr/event/melee-singles",
                "Melee Singles",
                Some("Super Smash Bros. Melee"),
            ),
            event(
                "tournament/fbr/event/ultimate-singles",
                "Ultimate Singles",
                Some("Super Smash Bros. Ultimate"),
            ),
            event("tournament/fbr/event/rivals", "Rivals", Some("Rivals of Aether II")),
        ];
        let text = generate_config("tournament/fbr", &events, &mapping(), &PathBuf::from("/tmp/data"));

        let config: SchedulerConfig = toml::from_str(&text).expect("generated config parses");
        config.validate().expect("generated config validates");
        assert_eq!(config.tournament_slug.as_deref(), Some("tournament/fbr"));
        assert_eq!(config.brackets.len(), 3);
        assert_eq!(config.brackets[0].setup_type, Some(OneOrMany::One("crt".to_owned())));
        assert_eq!(config.brackets[0].duration_prior_secs, 870);
        assert_eq!(
            config.brackets[1].setup_type,
            Some(OneOrMany::Many(vec!["switch".to_owned(), "pokemon".to_owned()]))
        );
        assert_eq!(config.brackets[2].setup_type, None, "unmatched game keeps the default pool");
        assert_eq!(config.known_called_state_int, Some(6));
        assert!(config.state_file.as_deref().is_some_and(|p| p.ends_with("fbr-state.json")));
    }

    #[test]
    fn template_parses_as_an_empty_mapping() {
        let setups: GameSetups = toml::from_str(super::GAME_SETUPS_TEMPLATE).unwrap();
        assert!(setups.game.is_empty());
    }
}

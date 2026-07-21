//! `--init-tournament`: one command from a start.gg tournament URL to a
//! ready-to-review `scheduler.toml`.
//!
//! The generated config carries only what is tournament-specific: the event
//! list (with each event's setup types, looked up from the operator's global
//! game mapping), the identity pin, and per-tournament state-file paths.
//! Station counts deliberately stay OUT of it — the 's' modal owns those at
//! runtime and persists them to the cross-tournament defaults file.

use std::{collections::BTreeMap, fmt::Write as _, fs, path::Path, sync::LazyLock};

use bracket_tools_startgg::EventInfo;
use serde::Deserialize;
use thiserror::Error;

/// The operator's global game mapping, in the XDG config dir next to
/// `scheduler.toml`.
pub const GAME_SETUPS_FILE: &str = "game-setups.toml";

/// The standard entries for the games this scene actually runs. Compiled in:
/// lookup falls back here when the operator's file has no matching entry, and
/// the auto-written template starts from these instead of blanks.
pub const BUILTIN_GAME_SETUPS: &str = r#"[game.smash] # every "Super Smash Bros. ..." title
setup_types = ["switch", "pokemon"]

[game.rivals]
setup_types = ["pc"]

[game."m.u.g.e.n"]
setup_types = ["pc"]

[game."pokémon"]
setup_types = ["pokemon"]
"#;

/// Explanatory header of the auto-written mapping file; the body is
/// [`BUILTIN_GAME_SETUPS`].
const GAME_SETUPS_HEADER: &str = r#"# game-setups.toml — maps start.gg videogames to the setup types their
# events run on. `--init-tournament` reads this to seed each generated
# event's `setup_type`.
#
# A table's key matches case-insensitively as a SUBSTRING of the start.gg
# videogame name, so [game.melee] matches "Super Smash Bros. Melee" (the
# longest matching key wins). Setup type names are your own labels; station
# counts per type live in setup-defaults.toml and the in-tool 's' modal,
# not here.
#
# The entries below are the built-in standards, which also apply when a game
# has no entry in this file at all. An entry here overrides its built-in;
# `setup_types = []` forces a game onto the shared default pool.
#
# Optional minutes seed each event's duration prior (used until live
# completions teach the model): prior = setup_minutes + 2.5 * game_minutes.
#
# [game.melee]
# setup_types = ["crt"]
# game_minutes = 5
# setup_minutes = 2
"#;

/// The full auto-written `game-setups.toml`: header + built-in standards.
pub fn game_setups_template() -> String {
    format!("{GAME_SETUPS_HEADER}\n{BUILTIN_GAME_SETUPS}")
}

/// A bo3 set averages about 2.5 games; the duration model is bo3-normalized,
/// so seeded priors use the same basis.
const BO3_AVERAGE_GAMES: f64 = 2.5;

static BUILTIN: LazyLock<GameSetups> = LazyLock::new(|| toml::from_str(BUILTIN_GAME_SETUPS).expect("built-in game mapping parses"));

/// Which mapping answered a game lookup — the generated config and the init
/// summary say so, so the operator knows where a pool assignment came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchSource {
    UserFile,
    Builtin,
}

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

    /// The operator's own entry when one matches, else the built-in standard.
    /// An operator entry always wins outright — even an empty one, which is
    /// how a game gets pinned to the shared default pool.
    pub fn match_game_or_builtin(&self, videogame: &str) -> Option<(&GameEntry, MatchSource)> {
        self.match_game(videogame)
            .map(|entry| (entry, MatchSource::UserFile))
            .or_else(|| BUILTIN.match_game(videogame).map(|entry| (entry, MatchSource::Builtin)))
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
        if let Some(game) = event.videogame.as_deref() {
            let _ = writeln!(out, "videogame = \"{}\"", game.replace('"', "\\\""));
        }
        match event.videogame.as_deref().and_then(|game| setups.match_game_or_builtin(game)) {
            Some((entry, source)) => {
                let provenance = match source {
                    MatchSource::UserFile => "",
                    MatchSource::Builtin => " # built-in standard (no game-setups.toml entry)",
                };
                match entry.setup_types.as_slice() {
                    [] => {}
                    [only] => {
                        let _ = writeln!(out, "setup_type = \"{only}\"{provenance}");
                    }
                    many => {
                        let list = many.iter().map(|t| format!("\"{t}\"")).collect::<Vec<_>>().join(", ");
                        let _ = writeln!(out, "setup_type = [{list}]{provenance}");
                    }
                }
                match entry.prior_secs() {
                    Some(prior) => {
                        let _ = writeln!(out, "duration_prior_secs = {prior} # seeded from game-setups.toml");
                    }
                    // Silence here reads as "8:00 forever" at the desk; say
                    // where the estimate comes from and how to improve it.
                    None => {
                        let _ = writeln!(
                            out,
                            "# duration_prior_secs = 480  # default 8m/bo3 — set game_minutes/setup_minutes\n\
                             #   in game-setups.toml and re-init (or edit here); live results refine it"
                        );
                    }
                }
            }
            None => {
                let _ = writeln!(
                    out,
                    "# no game-setups.toml or built-in entry matched — this event uses the shared default pool"
                );
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use bracket_tools_startgg::EventInfo;

    use super::{game_setups_template, generate_config, parse_tournament_slug, GameSetups, InitError, MatchSource};
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
    fn builtin_standards_cover_the_scene_games() {
        let empty = GameSetups::default();
        for (game, types) in [
            ("Super Smash Bros. Ultimate", vec!["switch", "pokemon"]),
            ("Super Smash Bros. Melee", vec!["switch", "pokemon"]),
            ("Rivals of Aether II", vec!["pc"]),
            ("M.U.G.E.N", vec!["pc"]),
            ("Pokémon Champions", vec!["pokemon"]),
        ] {
            let (entry, source) = empty.match_game_or_builtin(game).unwrap_or_else(|| panic!("{game} matches"));
            assert_eq!(entry.setup_types, types, "{game}");
            assert_eq!(source, MatchSource::Builtin, "{game}");
        }
        assert!(empty.match_game_or_builtin("Tetris Effect").is_none());
    }

    #[test]
    fn user_entries_beat_builtins_even_when_empty() {
        let setups = mapping();
        let (melee, source) = setups.match_game_or_builtin("Super Smash Bros. Melee").unwrap();
        assert_eq!(melee.setup_types, vec!["crt"], "user entry wins over the built-in smash standard");
        assert_eq!(source, MatchSource::UserFile);

        let pinned: GameSetups = toml::from_str("[game.smash]\nsetup_types = []").unwrap();
        let (entry, source) = pinned.match_game_or_builtin("Super Smash Bros. Melee").unwrap();
        assert!(entry.setup_types.is_empty(), "an empty user entry pins the default pool");
        assert_eq!(source, MatchSource::UserFile);
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
            event("tournament/fbr/event/tetris", "Tetris", Some("Tetris Effect")),
        ];
        let text = generate_config("tournament/fbr", &events, &mapping(), &PathBuf::from("/tmp/data"));

        let config: SchedulerConfig = toml::from_str(&text).expect("generated config parses");
        config.validate().expect("generated config validates");
        assert_eq!(config.tournament_slug.as_deref(), Some("tournament/fbr"));
        assert_eq!(config.brackets.len(), 4);
        assert_eq!(config.brackets[0].setup_type, Some(OneOrMany::One("crt".to_owned())));
        assert_eq!(config.brackets[0].duration_prior_secs, 870);
        assert_eq!(
            config.brackets[1].setup_type,
            Some(OneOrMany::Many(vec!["switch".to_owned(), "pokemon".to_owned()]))
        );
        assert_eq!(
            config.brackets[2].setup_type,
            Some(OneOrMany::One("pc".to_owned())),
            "no user entry — the built-in rivals standard fills in"
        );
        assert!(text.contains("built-in standard"), "built-in seeds are labeled as such");
        assert_eq!(config.brackets[3].setup_type, None, "unmatched game keeps the default pool");
        assert_eq!(config.known_called_state_int, Some(6));
        assert!(config.state_file.as_deref().is_some_and(|p| p.ends_with("fbr-state.json")));
    }

    #[test]
    fn template_carries_exactly_the_builtin_standards() {
        let template: GameSetups = toml::from_str(&game_setups_template()).unwrap();
        let builtin: GameSetups = toml::from_str(super::BUILTIN_GAME_SETUPS).unwrap();
        assert!(!builtin.game.is_empty());
        assert_eq!(template.game.keys().collect::<Vec<_>>(), builtin.game.keys().collect::<Vec<_>>());
    }
}

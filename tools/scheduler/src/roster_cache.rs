//! Per-event character-roster cache (`<data-dir>/rosters/<slug>.toml`).
//!
//! Rosters are static per videogame, so the first successful live fetch
//! persists the reporting vocabulary for later runs: a rate-limited relaunch
//! keeps its character picker, and a `--simulate` rehearsal of a captured
//! tournament wears the real cast instead of the fixture placeholder.
//! Best-effort throughout: any I/O or parse failure is just a cache miss.

use std::{
    fs,
    path::{Path, PathBuf},
};

use bracket_tools_startgg::CharacterInfo;
use serde::{Deserialize, Serialize};

const ROSTER_DIR: &str = "rosters";

#[derive(Serialize, Deserialize)]
struct RosterDoc {
    characters: Vec<CharacterInfo>,
}

pub fn load(data_dir: &Path, slug: &str) -> Option<Vec<CharacterInfo>> {
    let text = fs::read_to_string(roster_path(data_dir, slug)).ok()?;
    let doc: RosterDoc = toml::from_str(&text).ok()?;
    (!doc.characters.is_empty()).then_some(doc.characters)
}

pub fn save(data_dir: &Path, slug: &str, characters: &[CharacterInfo]) {
    if characters.is_empty() {
        return;
    }
    let doc = RosterDoc {
        characters: characters.to_vec(),
    };
    let Ok(text) = toml::to_string(&doc) else { return };
    let path = roster_path(data_dir, slug);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let tmp = path.with_extension("toml.tmp");
    if fs::write(&tmp, text).is_ok() {
        let _ = fs::rename(&tmp, &path);
    }
}

fn roster_path(data_dir: &Path, key: &str) -> PathBuf {
    let stem: String = key
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect();
    data_dir.join(ROSTER_DIR).join(format!("{stem}.toml"))
}

#[cfg(test)]
mod tests {
    use bracket_tools_startgg::CharacterInfo;

    use super::{load, save};

    #[test]
    fn round_trips_and_misses_cleanly() {
        let dir = std::env::temp_dir().join(format!("bt-roster-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let slug = "tournament/fbr/event/ultimate";

        assert_eq!(load(&dir, slug), None, "cold cache misses");

        let roster = vec![
            CharacterInfo {
                id: 1,
                name: "Mario".to_owned(),
            },
            CharacterInfo {
                id: 2,
                name: "Banjo & Kazooie".to_owned(),
            },
        ];
        save(&dir, slug, &roster);
        assert_eq!(load(&dir, slug), Some(roster));

        assert_eq!(load(&dir, "tournament/fbr/event/melee"), None, "slugs don't collide");
    }

    #[test]
    fn empty_rosters_are_not_persisted() {
        let dir = std::env::temp_dir().join(format!("bt-roster-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        save(&dir, "slug", &[]);
        assert_eq!(load(&dir, "slug"), None);
    }
}

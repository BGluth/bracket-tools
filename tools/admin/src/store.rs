//! Local persistence for `gg-admin`: the find-pool file (the default search
//! pool for `find`) and the per-tournament roster cache that keeps repeated
//! `find` runs off the network. The roster cache is best-effort — a corrupt
//! or missing file is simply a miss and never breaks a desk command.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use bracket_tools_startgg::{AdminParticipant, AdminTournament};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

const POOL_FILE: &str = "find-pool.toml";
const ROSTER_CACHE_DIR: &str = "admin-rosters";

/// The persisted default search pool: bare tournament slugs, insertion order.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Pool {
    #[serde(default)]
    pub tournaments: Vec<String>,
}

impl Pool {
    /// Adds a bare slug; true when it was new.
    pub fn add(&mut self, bare: &str) -> bool {
        if self.tournaments.iter().any(|t| t == bare) {
            false
        } else {
            self.tournaments.push(bare.to_string());
            true
        }
    }

    /// Removes a bare slug; true when it was present.
    pub fn remove(&mut self, bare: &str) -> bool {
        let before = self.tournaments.len();
        self.tournaments.retain(|t| t != bare);
        self.tournaments.len() != before
    }
}

pub fn pool_path() -> PathBuf {
    config_dir().join(POOL_FILE)
}

/// A missing file is an empty pool; a corrupt one is an error (never silently
/// clobber a hand-edited file).
pub fn load_pool() -> Result<Pool> {
    let path = pool_path();
    if !path.exists() {
        return Ok(Pool::default());
    }
    let raw = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;

    toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))
}

pub fn save_pool(pool: &Pool) -> Result<()> {
    let path = pool_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    fs::write(&path, toml::to_string_pretty(pool)?).with_context(|| format!("writing {}", path.display()))
}

/// One tournament's cached roster; rewritten on every full live fetch.
#[derive(Serialize, Deserialize)]
pub struct CachedRoster {
    pub header: AdminTournament,
    pub participants: Vec<AdminParticipant>,
    pub fetched_at: i64,
}

/// Best-effort: an unreadable or corrupt cache file is simply a miss.
pub fn load_cached_roster(bare: &str) -> Option<CachedRoster> {
    let raw = fs::read_to_string(roster_cache_path(bare)).ok()?;

    serde_json::from_str(&raw).ok()
}

/// Best-effort: failure to persist never breaks the command that fetched.
pub fn save_cached_roster(bare: &str, roster: &CachedRoster) {
    let path = roster_cache_path(bare);
    let created = path.parent().map(fs::create_dir_all);
    if !matches!(created, Some(Ok(()))) {
        return;
    }
    if let Ok(json) = serde_json::to_string(roster) {
        let _ = fs::write(path, json);
    }
}

fn roster_cache_path(bare: &str) -> PathBuf {
    data_dir().join(ROSTER_CACHE_DIR).join(format!("{bare}.json"))
}

fn config_dir() -> PathBuf {
    project_path(ProjectDirs::config_dir)
}

fn data_dir() -> PathBuf {
    project_path(ProjectDirs::data_dir)
}

/// Same XDG project the scheduler uses, so all bracket-tools state co-locates
/// under `~/.config/bracket-tools` / `~/.local/share/bracket-tools`.
fn project_path(pick: fn(&ProjectDirs) -> &Path) -> PathBuf {
    match ProjectDirs::from("", "", "bracket-tools") {
        Some(dirs) => pick(&dirs).to_path_buf(),
        None => PathBuf::from("."),
    }
}

#[cfg(test)]
mod tests {
    use super::Pool;

    #[test]
    fn pool_add_remove_dedups_and_preserves_order() {
        let mut pool = Pool::default();
        assert!(pool.add("fbr-99"));
        assert!(pool.add("fbr-100"));
        assert!(!pool.add("fbr-99"));
        assert_eq!(pool.tournaments, vec!["fbr-99", "fbr-100"]);

        assert!(pool.remove("fbr-99"));
        assert!(!pool.remove("fbr-99"));
        assert_eq!(pool.tournaments, vec!["fbr-100"]);
    }

    #[test]
    fn pool_toml_round_trip() {
        let mut pool = Pool::default();
        pool.add("french-bread-rumble-100");

        let raw = toml::to_string_pretty(&pool).unwrap();
        let back: Pool = toml::from_str(&raw).unwrap();
        assert_eq!(back.tournaments, pool.tournaments);

        let empty: Pool = toml::from_str("").unwrap();
        assert!(empty.tournaments.is_empty());
    }
}

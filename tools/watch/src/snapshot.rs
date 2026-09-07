//! The last sweep, persisted between ticks: the "before" of every diff and
//! the source of the feeds.

use std::{collections::BTreeMap, fs, path::Path};

use anyhow::{Context, Result};
use bracket_tools_startgg::{StartGgId, TournamentSummary};
use serde::{Deserialize, Serialize};

use crate::files::write_atomic;

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub taken_at: i64,
    pub tournaments: BTreeMap<StartGgId, TournamentSummary>,
}

impl Snapshot {
    pub fn from_sweep(taken_at: i64, tournaments: Vec<TournamentSummary>) -> Self {
        Self {
            taken_at,
            tournaments: tournaments.into_iter().map(|t| (t.id, t)).collect(),
        }
    }

    /// A missing file is a first run (`None`); a corrupt one is an error,
    /// never silently a first run.
    pub fn load(path: &Path) -> Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }
        let raw = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let snapshot = serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;

        Ok(Some(snapshot))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        write_atomic(path, serde_json::to_string_pretty(self)?.as_bytes())
    }

    /// Earliest start first; undated tournaments last; ties by id.
    pub fn by_start(&self) -> Vec<&TournamentSummary> {
        let mut ordered: Vec<&TournamentSummary> = self.tournaments.values().collect();
        ordered.sort_by_key(|t| (t.start_at.is_none(), t.start_at, t.id));
        ordered
    }
}

#[cfg(test)]
mod tests {
    use std::{env, fs, process};

    use super::Snapshot;
    use crate::test_support::summary;

    #[test]
    fn round_trips_through_json() {
        let dir = env::temp_dir().join(format!("gg-watch-snapshot-{}", process::id()));
        let path = dir.join("snapshot.json");
        let snapshot = Snapshot::from_sweep(5, vec![summary(2, "b-1", Some(20)), summary(1, "a-1", Some(10))]);

        assert!(Snapshot::load(&path).unwrap().is_none());
        snapshot.save(&path).unwrap();
        assert_eq!(Snapshot::load(&path).unwrap(), Some(snapshot));

        fs::write(&path, "{not json").unwrap();
        assert!(Snapshot::load(&path).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn orders_by_start_with_undated_last() {
        let snapshot = Snapshot::from_sweep(
            0,
            vec![
                summary(3, "c", None),
                summary(2, "b", Some(20)),
                summary(1, "a", Some(10)),
                summary(4, "d", Some(10)),
            ],
        );
        let ids: Vec<u64> = snapshot.by_start().iter().map(|t| t.id).collect();

        assert_eq!(ids, vec![1, 4, 2, 3]);
    }
}

//! Set-duration learning: per-bracket samples blended with a config prior,
//! plus the snapshot diff that discovers completions.
//!
//! Samples are bo3-normalized on ingest; consumers scale back up per round
//! (×5/3 for bo5). Ingest is idempotent per set: a sample is only replaced
//! when its source timestamps change (`startedAt` is overwritten by every
//! remote action, so re-polls of the same completed set are no-ops).
//! Called-at offset *estimation* is S3's; the offset is an input here.

use std::collections::HashMap;

use crate::{
    config::{DEFAULT_DURATION_PRIOR_SECS, DEFAULT_PRIOR_WEIGHT},
    conflict::{occupant_keys, AliasMap, ConflictKey, UnixMillis},
    model::{BracketId, LiveSet, SetId, SetKey},
};

/// Clamp bounds for raw durations: below smells like a walkover or an
/// immediate report; above smells like a set nobody closed out.
pub const MIN_SAMPLE_SECS: i64 = 60;
pub const MAX_SAMPLE_SECS: i64 = 45 * 60;

/// One completed set as the diff observed it (server timestamps, seconds).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedSet {
    pub key: SetKey,
    pub id: SetId,
    pub started_at: Option<i64>,
    pub completed_at: i64,
}

/// Everything one poll's completed-set diff produced, as data.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SnapshotDiff {
    /// Sets whose completion (or completion timestamps) is news — duration
    /// ingest material.
    pub completed: Vec<CompletedSet>,
    /// Newly-completed sets only (tombstone clearing, rest windows, UI
    /// toasts) — timestamp revisions don't re-fire.
    pub results_arrived: Vec<SetKey>,
    /// Per-key completion times (unix millis); fold with max into the rest
    /// window's `last_completed` map.
    pub last_completed: Vec<(ConflictKey, UnixMillis)>,
}

/// Diffs two snapshots of one bracket by swap-stable [`SetKey`].
pub fn diff_snapshots(prev: &[LiveSet], next: &[LiveSet], aliases: &AliasMap) -> SnapshotDiff {
    let prev_by_key: HashMap<&SetKey, &LiveSet> = prev.iter().map(|s| (&s.key, s)).collect();
    let mut diff = SnapshotDiff::default();

    for set in next {
        if !set.is_completed() {
            continue;
        }
        let before = prev_by_key.get(&set.key).copied();
        let newly = !before.is_some_and(LiveSet::is_completed);
        let timestamps_changed = before.is_some_and(|b| b.started_at != set.started_at || b.completed_at != set.completed_at);

        if newly {
            diff.results_arrived.push(set.key.clone());
        }
        // Winner-only completions (no completedAt) are results but carry no
        // time — nothing for durations or rest windows.
        let Some(completed_at) = set.completed_at else {
            continue;
        };
        if newly {
            for occupant in set.occupants() {
                for key in occupant_keys(occupant, aliases) {
                    diff.last_completed.push((key, completed_at * 1000));
                }
            }
        }
        if newly || timestamps_changed {
            diff.completed.push(CompletedSet {
                key: set.key.clone(),
                id: set.id.clone(),
                started_at: set.started_at,
                completed_at,
            });
        }
    }
    diff
}

#[derive(Debug, Clone, PartialEq)]
struct Sample {
    /// bo3-normalized seconds.
    duration_secs: f64,
    /// Source timestamps this sample came from; ingest is a no-op while they
    /// are unchanged.
    fingerprint: (Option<i64>, i64, Option<UnixMillis>),
}

#[derive(Debug, Clone)]
struct BracketDurations {
    prior_secs: f64,
    prior_weight: f64,
    samples: HashMap<SetId, Sample>,
}

impl BracketDurations {
    fn estimate_secs(&self) -> f64 {
        let total: f64 = self.samples.values().map(|s| s.duration_secs).sum();
        (self.prior_secs * self.prior_weight + total) / (self.prior_weight + self.samples.len() as f64)
    }
}

/// Per-bracket duration estimates: prior-seeded weighted mean over observed
/// samples.
#[derive(Debug, Clone, Default)]
pub struct DurationModel {
    brackets: HashMap<BracketId, BracketDurations>,
}

impl DurationModel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Seeds a bracket's prior (from config) before any samples arrive.
    pub fn configure_bracket(&mut self, bracket: BracketId, prior_secs: u64, prior_weight: f64) {
        self.brackets.insert(
            bracket,
            BracketDurations {
                prior_secs: prior_secs as f64,
                prior_weight,
                samples: HashMap::new(),
            },
        );
    }

    /// Ingests one completed set. `called_at` is our local call time (unix
    /// millis) and `offset_secs` the estimated call→start lag, used only when
    /// the server never saw a start. Returns whether a sample was added or
    /// replaced.
    pub fn ingest(
        &mut self,
        bracket: &BracketId,
        completed: &CompletedSet,
        best_of: Option<i32>,
        called_at: Option<UnixMillis>,
        offset_secs: i64,
    ) -> bool {
        let fingerprint = (completed.started_at, completed.completed_at, called_at);
        let durations = self.brackets.entry(bracket.clone()).or_insert_with(default_bracket);
        if durations.samples.get(&completed.id).is_some_and(|s| s.fingerprint == fingerprint) {
            return false;
        }

        let raw_secs = match (completed.started_at, called_at) {
            (Some(started), _) => completed.completed_at - started,
            (None, Some(called)) => completed.completed_at - (called / 1000 + offset_secs),
            (None, None) => return false,
        };
        let clamped = raw_secs.clamp(MIN_SAMPLE_SECS, MAX_SAMPLE_SECS) as f64;
        let normalized = clamped * 3.0 / best_of.unwrap_or(3).max(1) as f64;

        durations.samples.insert(
            completed.id.clone(),
            Sample {
                duration_secs: normalized,
                fingerprint,
            },
        );
        true
    }

    /// bo3-normalized estimate; unseen brackets answer with the default
    /// prior.
    pub fn estimate_secs(&self, bracket: &BracketId) -> f64 {
        self.brackets
            .get(bracket)
            .map_or(DEFAULT_DURATION_PRIOR_SECS as f64, BracketDurations::estimate_secs)
    }

    /// The simulator's per-round scaling: bo5 runs ×5/3.
    pub fn scaled_estimate_secs(&self, bracket: &BracketId, best_of: Option<i32>) -> f64 {
        self.estimate_secs(bracket) * best_of.unwrap_or(3).max(1) as f64 / 3.0
    }

    pub fn sample_count(&self, bracket: &BracketId) -> usize {
        self.brackets.get(bracket).map_or(0, |b| b.samples.len())
    }

    pub fn prior_weight(&self, bracket: &BracketId) -> f64 {
        self.brackets.get(bracket).map_or(DEFAULT_PRIOR_WEIGHT, |b| b.prior_weight)
    }

    /// True once any bracket has a real observation — the rollout HOLD gate.
    pub fn has_samples(&self) -> bool {
        self.brackets.values().any(|b| !b.samples.is_empty())
    }
}

fn default_bracket() -> BracketDurations {
    BracketDurations {
        prior_secs: DEFAULT_DURATION_PRIOR_SECS as f64,
        prior_weight: DEFAULT_PRIOR_WEIGHT,
        samples: HashMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::{diff_snapshots, CompletedSet, DurationModel, MAX_SAMPLE_SECS, MIN_SAMPLE_SECS};
    use crate::{
        conflict::{AliasMap, ConflictKey},
        model::{BracketId, PlayerId, SetId, SetKey},
        synth::{complete, make_de_bracket, materialize_ids},
    };

    const T0: i64 = 1_751_000_000;

    fn bracket_id() -> BracketId {
        BracketId("melee".to_owned())
    }

    fn completed(id: &str, started_at: Option<i64>, completed_at: i64) -> CompletedSet {
        CompletedSet {
            key: SetKey {
                phase_group: "77".to_owned(),
                round: 1,
                identifier: id.to_owned(),
            },
            id: SetId(id.to_owned()),
            started_at,
            completed_at,
        }
    }

    fn seeded_model() -> DurationModel {
        let mut model = DurationModel::new();
        model.configure_bracket(bracket_id(), 480, 4.0);
        model
    }

    #[test]
    fn prior_blends_with_samples() {
        let mut model = seeded_model();
        assert_eq!(model.estimate_secs(&bracket_id()), 480.0);

        assert!(model.ingest(&bracket_id(), &completed("A", Some(T0), T0 + 600), Some(3), None, 0));
        assert_eq!(model.estimate_secs(&bracket_id()), (480.0 * 4.0 + 600.0) / 5.0);
        assert_eq!(model.sample_count(&bracket_id()), 1);
        assert_eq!(model.prior_weight(&bracket_id()), 4.0);
        assert!(model.has_samples());
    }

    #[test]
    fn reingest_is_idempotent_until_timestamps_change() {
        let mut model = seeded_model();
        let set = completed("A", Some(T0), T0 + 600);
        assert!(model.ingest(&bracket_id(), &set, Some(3), None, 0));
        assert!(!model.ingest(&bracket_id(), &set, Some(3), None, 0), "same timestamps: no-op");
        let estimate = model.estimate_secs(&bracket_id());

        // A timestamp revision replaces (not duplicates) the sample.
        let revised = completed("A", Some(T0), T0 + 900);
        assert!(model.ingest(&bracket_id(), &revised, Some(3), None, 0));
        assert_eq!(model.sample_count(&bracket_id()), 1);
        assert!(model.estimate_secs(&bracket_id()) > estimate);
    }

    #[test]
    fn outliers_clamp_to_bounds() {
        let mut model = seeded_model();
        model.ingest(&bracket_id(), &completed("A", Some(T0), T0 + 5), Some(3), None, 0);
        model.ingest(&bracket_id(), &completed("B", Some(T0), T0 + 7200), Some(3), None, 0);
        let expected = (480.0 * 4.0 + MIN_SAMPLE_SECS as f64 + MAX_SAMPLE_SECS as f64) / 6.0;
        assert_eq!(model.estimate_secs(&bracket_id()), expected);
    }

    #[test]
    fn bo5_samples_normalize_and_estimates_scale_back() {
        let mut model = seeded_model();
        model.ingest(&bracket_id(), &completed("A", Some(T0), T0 + 1000), Some(5), None, 0);
        // Stored bo3-normalized: 1000 * 3/5 = 600.
        assert_eq!(model.estimate_secs(&bracket_id()), (480.0 * 4.0 + 600.0) / 5.0);

        let bo3 = model.scaled_estimate_secs(&bracket_id(), Some(3));
        let bo5 = model.scaled_estimate_secs(&bracket_id(), Some(5));
        assert_eq!(bo5, bo3 * 5.0 / 3.0);
    }

    #[test]
    fn called_at_fallback_and_skip() {
        let mut model = seeded_model();

        // No start and no call: unusable.
        assert!(!model.ingest(&bracket_id(), &completed("A", None, T0 + 600), Some(3), None, 30));
        assert_eq!(model.sample_count(&bracket_id()), 0);

        // completedAt − (called_at + offset) = (T0+600) − (T0+30) = 570.
        assert!(model.ingest(&bracket_id(), &completed("A", None, T0 + 600), Some(3), Some(T0 * 1000), 30));
        assert_eq!(model.estimate_secs(&bracket_id()), (480.0 * 4.0 + 570.0) / 5.0);
    }

    #[test]
    fn unconfigured_bracket_answers_with_defaults() {
        let model = DurationModel::new();
        let unknown = BracketId("unknown".to_owned());
        assert_eq!(model.estimate_secs(&unknown), 480.0);
        assert!(!model.has_samples());
    }

    #[test]
    fn diff_reports_new_completions_with_conflict_keys() {
        let aliases = AliasMap::default();
        let bracket = make_de_bracket(77, 4);
        let mut next = bracket.sets.clone();
        complete(&mut next[0], 0, T0 + 600);
        next[0].started_at = Some(T0);

        let diff = diff_snapshots(&bracket.sets, &next, &aliases);
        assert_eq!(diff.completed.len(), 1);
        assert_eq!(diff.completed[0].started_at, Some(T0));
        assert_eq!(diff.completed[0].completed_at, T0 + 600);
        assert_eq!(diff.results_arrived, vec![next[0].key.clone()]);
        // Both R1 occupants get last-completed updates, in millis.
        let mut keys = diff.last_completed.to_vec();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                (ConflictKey::Player(PlayerId("P1".to_owned())), (T0 + 600) * 1000),
                (ConflictKey::Player(PlayerId("P4".to_owned())), (T0 + 600) * 1000),
            ]
        );
    }

    #[test]
    fn diff_timestamp_revision_reingests_without_refiring_results() {
        let aliases = AliasMap::default();
        let bracket = make_de_bracket(77, 4);
        let mut prev = bracket.sets.clone();
        complete(&mut prev[0], 0, T0 + 600);
        let mut next = prev.clone();

        assert_eq!(diff_snapshots(&prev, &next, &aliases), Default::default(), "no change, no diff");

        next[0].started_at = Some(T0 + 60);
        let diff = diff_snapshots(&prev, &next, &aliases);
        assert_eq!(diff.completed.len(), 1, "revised timestamps re-feed ingest");
        assert!(diff.results_arrived.is_empty(), "but the result already arrived");
        assert!(diff.last_completed.is_empty());
    }

    #[test]
    fn diff_survives_the_preview_to_numeric_swap() {
        let aliases = AliasMap::default();
        let bracket = make_de_bracket(77, 4);
        let mut next = materialize_ids(&bracket.sets, 5000);
        complete(&mut next[1], 0, T0 + 600);

        let diff = diff_snapshots(&bracket.sets, &next, &aliases);
        assert_eq!(
            diff.completed.len(),
            1,
            "keys match across the id swap: only the real completion diffs"
        );
        assert_eq!(diff.completed[0].id, next[1].id, "carries the new numeric id");
    }

    #[test]
    fn winner_only_completion_is_a_result_but_not_duration_material() {
        let aliases = AliasMap::default();
        let bracket = make_de_bracket(77, 4);
        let mut next = bracket.sets.clone();
        next[0].winner_id = next[0].slots[0].occupant.as_ref().map(|o| o.entrant_id.clone());

        let diff = diff_snapshots(&bracket.sets, &next, &aliases);
        assert_eq!(diff.results_arrived, vec![next[0].key.clone()], "the result still arrived");
        assert!(diff.completed.is_empty(), "no completedAt, nothing to ingest");
        assert!(diff.last_completed.is_empty(), "no time, no rest window");
    }
}

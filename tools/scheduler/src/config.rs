//! Pure config structs. S2 only defines the shapes (serde derives are free);
//! the TOML file read arrives with the S3 wiring.

use serde::{Deserialize, Serialize};

use crate::model::{BracketId, GroupKind, PlayerId};

pub const DEFAULT_DURATION_PRIOR_SECS: u64 = 480;
pub const DEFAULT_PRIOR_WEIGHT: f64 = 4.0;
pub const DEFAULT_NOISE_EPSILON: f64 = 0.05;
pub const DEFAULT_REST_SIM_HORIZON_SECS: u64 = 600;

/// A physical station at the venue, identified by its position in the TO's
/// numbering (setup 1, setup 2, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SetupId(pub u32);

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SchedulerConfig {
    pub brackets: Vec<BracketConfig>,
    /// Every station at the desk's disposal. Per-bracket pools reference these.
    pub setups: Vec<SetupId>,
    /// Sets of player ids known to be the same human; merged into one conflict
    /// key each.
    #[serde(default)]
    pub player_aliases: Vec<Vec<PlayerId>>,
    /// Minimum seconds a player rests after finishing a set before being
    /// callable again.
    #[serde(default)]
    pub rest_window_secs: u64,
    /// When true, an unpinned state-int deviation escalates from an advisory
    /// flag to soft-busy evidence.
    #[serde(default)]
    pub escalate_unpinned_state_deviation: bool,
    #[serde(default)]
    pub sim: SimConfig,
}

/// One scheduled bracket (a start.gg event).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BracketConfig {
    /// The event slug, e.g. `tournament/french-bread-rumble-100/event/melee-singles`.
    pub slug: String,
    /// Preflight expectation only — the graph branches on the structure
    /// query's per-phase-group bracket type, not on this.
    pub expected_kind: Option<ExpectedKind>,
    #[serde(default)]
    pub mode: BracketMode,
    /// Unix seconds; overrides the structure query's start time.
    pub start_at_override: Option<i64>,
    /// Prior mean bo3 set duration, blended with observed samples.
    #[serde(default = "default_duration_prior_secs")]
    pub duration_prior_secs: u64,
    /// How many samples' worth of weight the prior carries.
    #[serde(default = "default_prior_weight")]
    pub prior_weight: f64,
    /// Setups this bracket may be called on. Pools may overlap across brackets.
    #[serde(default)]
    pub pool: Vec<SetupId>,
}

impl BracketConfig {
    /// Test/default-heavy constructor; fields are tuned per bracket in the
    /// real config file.
    pub fn new(slug: impl Into<String>) -> Self {
        Self {
            slug: slug.into(),
            expected_kind: None,
            mode: BracketMode::default(),
            start_at_override: None,
            duration_prior_secs: DEFAULT_DURATION_PRIOR_SECS,
            prior_weight: DEFAULT_PRIOR_WEIGHT,
            pool: Vec::new(),
        }
    }

    pub fn id(&self) -> BracketId {
        BracketId(self.slug.clone())
    }
}

/// How much scheduling a bracket gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BracketMode {
    /// Ranked, called, and simulated.
    #[default]
    Full,
    /// Feeds the conflict index (its players count as busy) but is never
    /// called or ranked by us.
    ConflictOnly,
}

/// Config-declared bracket shape, asserted against the live structure during
/// preflight (a mismatch is a warning, not a branch).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExpectedKind {
    Elimination,
    RoundRobin,
    Swiss,
}

impl ExpectedKind {
    pub fn matches(&self, kind: &GroupKind) -> bool {
        matches!(
            (self, kind),
            (Self::Elimination, GroupKind::Elimination)
                | (Self::RoundRobin, GroupKind::RoundRobin)
                | (Self::Swiss, GroupKind::Swiss { .. })
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimConfig {
    /// Relative makespan band within which two projected outcomes count as a
    /// tie (rollout HOLD gating and greedy fallback).
    #[serde(default = "default_noise_epsilon")]
    pub noise_epsilon: f64,
    /// How long the simulator keeps a resting player busy before assuming
    /// they return.
    #[serde(default = "default_rest_sim_horizon_secs")]
    pub rest_sim_horizon_secs: u64,
}

impl Default for SimConfig {
    fn default() -> Self {
        Self {
            noise_epsilon: DEFAULT_NOISE_EPSILON,
            rest_sim_horizon_secs: DEFAULT_REST_SIM_HORIZON_SECS,
        }
    }
}

fn default_duration_prior_secs() -> u64 {
    DEFAULT_DURATION_PRIOR_SECS
}

fn default_prior_weight() -> f64 {
    DEFAULT_PRIOR_WEIGHT
}

fn default_noise_epsilon() -> f64 {
    DEFAULT_NOISE_EPSILON
}

fn default_rest_sim_horizon_secs() -> u64 {
    DEFAULT_REST_SIM_HORIZON_SECS
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{BracketMode, ExpectedKind, SchedulerConfig, DEFAULT_DURATION_PRIOR_SECS, DEFAULT_REST_SIM_HORIZON_SECS};
    use crate::model::GroupKind;

    #[test]
    fn sparse_config_fills_defaults() {
        let config: SchedulerConfig = serde_json::from_value(json!({
            "brackets": [{ "slug": "tournament/t/event/melee", "expected_kind": null, "start_at_override": null }],
            "setups": [1, 2, 3],
        }))
        .unwrap();

        let bracket = &config.brackets[0];
        assert_eq!(bracket.mode, BracketMode::Full);
        assert_eq!(bracket.duration_prior_secs, DEFAULT_DURATION_PRIOR_SECS);
        assert_eq!(config.rest_window_secs, 0);
        assert!(!config.escalate_unpinned_state_deviation);
        assert_eq!(config.sim.rest_sim_horizon_secs, DEFAULT_REST_SIM_HORIZON_SECS);
    }

    #[test]
    fn expected_kind_is_an_assertion_helper() {
        assert!(ExpectedKind::Elimination.matches(&GroupKind::Elimination));
        assert!(ExpectedKind::Swiss.matches(&GroupKind::Swiss { num_rounds: 4 }));
        assert!(!ExpectedKind::RoundRobin.matches(&GroupKind::Elimination));
        assert!(!ExpectedKind::Elimination.matches(&GroupKind::Unsupported("MATCHMAKING".to_owned())));
    }
}

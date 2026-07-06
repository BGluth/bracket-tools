//! Scheduler configuration: the pure shapes (S2) plus the TOML file load and
//! validation that back the `--config` flag (S3).

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::model::{BracketId, GroupKind, PlayerId};

pub const DEFAULT_DURATION_PRIOR_SECS: u64 = 480;
pub const DEFAULT_PRIOR_WEIGHT: f64 = 4.0;
pub const DEFAULT_NOISE_EPSILON: f64 = 0.05;
pub const DEFAULT_REST_SIM_HORIZON_SECS: u64 = 600;
pub const DEFAULT_POLL_INTERVAL_SECS: u64 = 30;
pub const DEFAULT_NO_SHOW_SECS: u64 = 300;
pub const DEFAULT_STALE_WARN_POLLS: u32 = 3;
pub const DEFAULT_PER_PAGE: u32 = 50;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read config file {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse config file {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: Box<toml::de::Error>,
    },

    #[error("config lists no brackets")]
    NoBrackets,

    #[error("duplicate bracket slug {0:?}")]
    DuplicateSlug(String),

    #[error("duplicate setup id {}", (.0).0)]
    DuplicateSetup(SetupId),

    #[error("bracket {slug:?} is mode=full but has an empty setup pool")]
    EmptyPool { slug: String },

    #[error("bracket {slug:?} pools setup {} which is not in the top-level setups list", setup.0)]
    UnknownSetupInPool { slug: String, setup: SetupId },
}

/// A physical station at the venue, identified by its position in the TO's
/// numbering (setup 1, setup 2, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SetupId(pub u32);

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Seconds between full poll cycles.
    #[serde(default = "default_poll_interval_secs")]
    pub poll_interval_secs: u64,
    /// Seconds a called set may sit unstarted before the no-show alert.
    #[serde(default = "default_no_show_secs")]
    pub no_show_secs: u64,
    /// Consecutive failed cycles before an event's staleness escalates from
    /// badge to warning.
    #[serde(default = "default_stale_warn_polls")]
    pub stale_warn_polls: u32,
    /// Page size for set fetches.
    #[serde(default = "default_per_page")]
    pub per_page: u32,
    /// File containing the start.gg API token; see `cli` for the full
    /// resolution order.
    #[serde(default)]
    pub token_file: Option<PathBuf>,
    /// Asserted against every event's owning tournament during preflight when
    /// set.
    #[serde(default)]
    pub tournament_slug: Option<String>,
    /// Never arm writes, regardless of what the admin probe finds.
    #[serde(default)]
    pub advisor_only: bool,
    /// State ints that never count as a deviation. Defaults to the normal
    /// lifecycle vocabulary observed live (1=pending, 2=in-progress,
    /// 3=completed); CALLED is pinned separately.
    #[serde(default = "default_benign_state_ints")]
    pub known_benign_state_ints: Vec<i32>,
    /// Pinned CALLED state int (live-observed: 6). Learned from write
    /// responses when unset.
    #[serde(default)]
    pub known_called_state_int: Option<i32>,
    /// Pinned IN_PROGRESS state int (live-observed: 2). Learned from write
    /// responses when unset.
    #[serde(default)]
    pub known_in_progress_state_int: Option<i32>,
    /// Overlay (local operator state) path; defaults to ./scheduler-state.json
    /// beside the tool. The single-instance lockfile lives beside it.
    #[serde(default)]
    pub state_file: Option<PathBuf>,
    /// Last-good snapshot (offline cold-start seed); defaults to
    /// ./scheduler-snapshot.json.
    #[serde(default)]
    pub snapshot_file: Option<PathBuf>,
    // Remaining journaling paths land with their features. TODO(S4):
    // log_file, capture_dir.
    #[serde(default)]
    pub log_file: Option<PathBuf>,
    #[serde(default)]
    pub capture_dir: Option<PathBuf>,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            brackets: Vec::new(),
            setups: Vec::new(),
            player_aliases: Vec::new(),
            rest_window_secs: 0,
            escalate_unpinned_state_deviation: false,
            sim: SimConfig::default(),
            poll_interval_secs: DEFAULT_POLL_INTERVAL_SECS,
            no_show_secs: DEFAULT_NO_SHOW_SECS,
            stale_warn_polls: DEFAULT_STALE_WARN_POLLS,
            per_page: DEFAULT_PER_PAGE,
            token_file: None,
            tournament_slug: None,
            advisor_only: false,
            known_benign_state_ints: default_benign_state_ints(),
            known_called_state_int: None,
            known_in_progress_state_int: None,
            state_file: None,
            snapshot_file: None,
            log_file: None,
            capture_dir: None,
        }
    }
}

impl SchedulerConfig {
    /// Reads, parses, and validates a TOML config file.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let raw = fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_owned(),
            source,
        })?;
        let config: Self = toml::from_str(&raw).map_err(|source| ConfigError::Parse {
            path: path.to_owned(),
            source: Box::new(source),
        })?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.brackets.is_empty() {
            return Err(ConfigError::NoBrackets);
        }

        let mut setups = HashSet::new();
        for setup in &self.setups {
            if !setups.insert(*setup) {
                return Err(ConfigError::DuplicateSetup(*setup));
            }
        }

        let mut slugs = HashSet::new();
        for bracket in &self.brackets {
            if !slugs.insert(bracket.slug.as_str()) {
                return Err(ConfigError::DuplicateSlug(bracket.slug.clone()));
            }
            if bracket.mode == BracketMode::Full && bracket.pool.is_empty() {
                return Err(ConfigError::EmptyPool {
                    slug: bracket.slug.clone(),
                });
            }
            for setup in &bracket.pool {
                if !setups.contains(setup) {
                    return Err(ConfigError::UnknownSetupInPool {
                        slug: bracket.slug.clone(),
                        setup: *setup,
                    });
                }
            }
        }

        Ok(())
    }
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
#[serde(rename_all = "snake_case")]
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
#[serde(rename_all = "snake_case")]
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

fn default_poll_interval_secs() -> u64 {
    DEFAULT_POLL_INTERVAL_SECS
}

fn default_no_show_secs() -> u64 {
    DEFAULT_NO_SHOW_SECS
}

fn default_stale_warn_polls() -> u32 {
    DEFAULT_STALE_WARN_POLLS
}

fn default_per_page() -> u32 {
    DEFAULT_PER_PAGE
}

fn default_benign_state_ints() -> Vec<i32> {
    vec![1, 2, 3]
}

fn default_rest_sim_horizon_secs() -> u64 {
    DEFAULT_REST_SIM_HORIZON_SECS
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::{
        BracketConfig, BracketMode, ConfigError, ExpectedKind, SchedulerConfig, SetupId, DEFAULT_DURATION_PRIOR_SECS, DEFAULT_PER_PAGE,
        DEFAULT_POLL_INTERVAL_SECS, DEFAULT_REST_SIM_HORIZON_SECS,
    };
    use crate::model::GroupKind;

    fn valid_config() -> SchedulerConfig {
        SchedulerConfig {
            brackets: vec![BracketConfig {
                pool: vec![SetupId(1), SetupId(2)],
                ..BracketConfig::new("tournament/t/event/melee")
            }],
            setups: vec![SetupId(1), SetupId(2)],
            ..SchedulerConfig::default()
        }
    }

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
        assert_eq!(config.poll_interval_secs, DEFAULT_POLL_INTERVAL_SECS);
        assert_eq!(config.per_page, DEFAULT_PER_PAGE);
        assert!(!config.advisor_only);
        assert_eq!(config.known_called_state_int, None);
    }

    #[test]
    fn default_impl_matches_serde_defaults() {
        let sparse: SchedulerConfig = serde_json::from_value(json!({ "brackets": [], "setups": [] })).unwrap();
        let manual = SchedulerConfig::default();
        assert_eq!(sparse.poll_interval_secs, manual.poll_interval_secs);
        assert_eq!(sparse.no_show_secs, manual.no_show_secs);
        assert_eq!(sparse.stale_warn_polls, manual.stale_warn_polls);
        assert_eq!(sparse.per_page, manual.per_page);
    }

    #[test]
    fn full_toml_round_trip() {
        let toml_src = r#"
            setups = [1, 2, 3, 4]
            rest_window_secs = 240
            poll_interval_secs = 20
            advisor_only = true
            known_called_state_int = 6
            known_in_progress_state_int = 2
            known_benign_state_ints = [1]
            tournament_slug = "tournament/french-bread-rumble-100"
            token_file = "/tmp/token"
            player_aliases = [["111", "222"]]

            [[brackets]]
            slug = "tournament/french-bread-rumble-100/event/melee-singles"
            expected_kind = "elimination"
            pool = [1, 2]
            duration_prior_secs = 600

            [[brackets]]
            slug = "tournament/french-bread-rumble-100/event/pokemon-champions-4v4-double-battle"
            expected_kind = "swiss"
            mode = "conflict_only"
        "#;
        let config: SchedulerConfig = toml::from_str(toml_src).unwrap();
        config.validate().unwrap();

        assert_eq!(config.setups.len(), 4);
        assert_eq!(config.poll_interval_secs, 20);
        assert!(config.advisor_only);
        assert_eq!(config.known_called_state_int, Some(6));
        assert_eq!(config.token_file, Some(PathBuf::from("/tmp/token")));
        assert_eq!(config.brackets[0].expected_kind, Some(ExpectedKind::Elimination));
        assert_eq!(config.brackets[0].duration_prior_secs, 600);
        assert_eq!(config.brackets[1].mode, BracketMode::ConflictOnly);
        assert!(config.brackets[1].pool.is_empty());
    }

    #[test]
    fn validate_accepts_valid_config() {
        valid_config().validate().unwrap();
    }

    #[test]
    fn validate_rejects_no_brackets() {
        let config = SchedulerConfig {
            brackets: Vec::new(),
            ..valid_config()
        };
        assert!(matches!(config.validate(), Err(ConfigError::NoBrackets)));
    }

    #[test]
    fn validate_rejects_duplicate_slugs() {
        let mut config = valid_config();
        let duplicate = config.brackets[0].clone();
        config.brackets.push(duplicate);
        assert!(matches!(config.validate(), Err(ConfigError::DuplicateSlug(_))));
    }

    #[test]
    fn validate_rejects_duplicate_setups() {
        let mut config = valid_config();
        config.setups.push(SetupId(1));
        assert!(matches!(config.validate(), Err(ConfigError::DuplicateSetup(SetupId(1)))));
    }

    #[test]
    fn validate_rejects_empty_pool_on_full_bracket() {
        let mut config = valid_config();
        config.brackets[0].pool.clear();
        assert!(matches!(config.validate(), Err(ConfigError::EmptyPool { .. })));
    }

    #[test]
    fn validate_allows_empty_pool_on_conflict_only_bracket() {
        let mut config = valid_config();
        config.brackets[0].mode = BracketMode::ConflictOnly;
        config.brackets[0].pool.clear();
        config.validate().unwrap();
    }

    #[test]
    fn validate_rejects_pool_setup_missing_from_setups() {
        let mut config = valid_config();
        config.brackets[0].pool.push(SetupId(99));
        assert!(matches!(
            config.validate(),
            Err(ConfigError::UnknownSetupInPool { setup: SetupId(99), .. })
        ));
    }

    #[test]
    fn expected_kind_is_an_assertion_helper() {
        assert!(ExpectedKind::Elimination.matches(&GroupKind::Elimination));
        assert!(ExpectedKind::Swiss.matches(&GroupKind::Swiss { num_rounds: 4 }));
        assert!(!ExpectedKind::RoundRobin.matches(&GroupKind::Elimination));
        assert!(!ExpectedKind::Elimination.matches(&GroupKind::Unsupported("MATCHMAKING".to_owned())));
    }
}

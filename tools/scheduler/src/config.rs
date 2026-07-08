//! Scheduler configuration: the pure shapes (S2) plus the TOML file load and
//! validation that back the `--config` flag (S3).

use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::model::{BracketId, GroupKind, PlayerId};

/// The commented starter config written when live mode finds no config file.
/// Parses and validates as-is, so an edited copy runs immediately.
pub const STARTER_TEMPLATE: &str = r#"# scheduler — starter config (auto-created because none was found).
#
# Fill in your tournament's events and setups, then rerun:
#
#   scheduler --config <this file>
#
# Tip: `scheduler --simulate <captures-dir>` and `--synth <spec>` need no
# config at all — they derive one (add --pace N to play a rehearsal).

# Safety pin: keep true until you trust the tool with writes. Advisor-only
# sessions never mutate start.gg regardless of token permissions.
advisor_only = true

# Every station at the desk's disposal, in the TO's numbering.
setups = [1, 2, 3, 4]

# Seconds between full poll cycles; don't go below ~15 with several events.
poll_interval_secs = 30

# Seconds a called set may sit unstarted before the no-show alert.
no_show_secs = 300

# Minimum seconds a player rests after finishing a set before being callable.
rest_window_secs = 300

# Asserted against every event's owning tournament during preflight.
#tournament_slug = "tournament/your-tournament"

# File containing the start.gg API token (the --token flag and STARTGG_TOKEN
# environment variable both override this).
#token_file = "~/path/to/token"

# Crash-recovery state: the local overlay and the last-good snapshot. They
# default beside the working directory; the single-instance lockfile lives
# beside the state file.
#state_file = "scheduler-state.json"
#snapshot_file = "scheduler-snapshot.json"

# Live-observed start.gg state ints; leave pinned unless start.gg changes.
known_called_state_int = 6
known_in_progress_state_int = 2

# Same-human links across events, by player id: fill from the preflight
# identity-split report.
#player_aliases = [["1234567", "7654321"]]

# One [[brackets]] block per event at the desk.
[[brackets]]
slug = "tournament/your-tournament/event/your-main-event"
# Preflight warns if the live bracket isn't this shape:
# "elimination" | "round_robin" | "swiss"
expected_kind = "elimination"
# Setups this event may be called on (a subset of `setups` above).
pool = [1, 2, 3, 4]
# Prior mean bo3 set duration in seconds, blended with observed samples.
#duration_prior_secs = 480

# A second event: mode = "conflict_only" tracks its players as busy but never
# calls or ranks its sets.
#[[brackets]]
#slug = "tournament/your-tournament/event/your-side-event"
#expected_kind = "swiss"
#mode = "conflict_only"
"#;

pub const DEFAULT_DURATION_PRIOR_SECS: u64 = 480;
pub const DEFAULT_PRIOR_WEIGHT: f64 = 4.0;
pub const DEFAULT_NOISE_EPSILON: f64 = 0.05;
pub const DEFAULT_REST_SIM_HORIZON_SECS: u64 = 600;
/// Ceiling for `sim.duration_noise`: past this the multiplier band touches
/// zero and estimates stop meaning anything.
pub const MAX_DURATION_NOISE: f64 = 0.9;
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

    #[error("sim.duration_noise must be within [0.0, {MAX_DURATION_NOISE}], got {0}")]
    DurationNoiseOutOfRange(f64),
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
    /// Like [`Self::load`], but a missing file is `None` instead of an error
    /// (the caller decides how to bootstrap one).
    pub fn load_if_present(path: &Path) -> Result<Option<Self>, ConfigError> {
        match Self::load(path) {
            Ok(config) => Ok(Some(config)),
            Err(ConfigError::Read { ref source, .. }) if source.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

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
        if !(0.0..=MAX_DURATION_NOISE).contains(&self.sim.duration_noise) {
            return Err(ConfigError::DurationNoiseOutOfRange(self.sim.duration_noise));
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

/// Writes [`STARTER_TEMPLATE`] to `path` for the user to edit.
pub fn write_starter_template(path: &Path) -> io::Result<()> {
    fs::write(path, STARTER_TEMPLATE)
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
    /// Fractional spread applied to simulated set durations: each set gets a
    /// fixed, seed-derived multiplier in `1 ± duration_noise`. Zero (the
    /// default) keeps the sim fully smooth; mainly for `--autoplay`/`--pace`
    /// rehearsals, where organic variance makes the drill more realistic.
    #[serde(default)]
    pub duration_noise: f64,
    /// Seed for `duration_noise`; same seed + world = identical run.
    #[serde(default)]
    pub noise_seed: u64,
}

impl Default for SimConfig {
    fn default() -> Self {
        Self {
            noise_epsilon: DEFAULT_NOISE_EPSILON,
            rest_sim_horizon_secs: DEFAULT_REST_SIM_HORIZON_SECS,
            duration_noise: 0.0,
            noise_seed: 0,
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
    use std::{env, fs, path::PathBuf, process};

    use serde_json::json;

    use super::{
        write_starter_template, BracketConfig, BracketMode, ConfigError, ExpectedKind, SchedulerConfig, SetupId,
        DEFAULT_DURATION_PRIOR_SECS, DEFAULT_PER_PAGE, DEFAULT_POLL_INTERVAL_SECS, DEFAULT_REST_SIM_HORIZON_SECS, STARTER_TEMPLATE,
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
    fn duration_noise_defaults_off_and_validates_its_band() {
        let config = valid_config();
        assert_eq!(config.sim.duration_noise, 0.0);
        assert_eq!(config.sim.noise_seed, 0);

        let mut config = valid_config();
        config.sim.duration_noise = 0.9;
        config.validate().unwrap();
        config.sim.duration_noise = 0.91;
        assert!(matches!(config.validate(), Err(ConfigError::DurationNoiseOutOfRange(_))));
        config.sim.duration_noise = -0.1;
        assert!(matches!(config.validate(), Err(ConfigError::DurationNoiseOutOfRange(_))));
    }

    #[test]
    fn starter_template_parses_and_validates() {
        let config: SchedulerConfig = toml::from_str(STARTER_TEMPLATE).unwrap();
        config.validate().unwrap();
        assert!(config.advisor_only, "the template is safe by default");
        assert_eq!(config.known_called_state_int, Some(6));
    }

    #[test]
    fn load_if_present_distinguishes_missing_from_broken() {
        assert!(matches!(
            SchedulerConfig::load_if_present(&PathBuf::from("/nonexistent/scheduler.toml")),
            Ok(None)
        ));

        let dir = env::temp_dir().join(format!("scheduler-config-test-{}", process::id()));
        fs::create_dir_all(&dir).unwrap();
        let good = dir.join("good.toml");
        write_starter_template(&good).unwrap();
        assert!(matches!(SchedulerConfig::load_if_present(&good), Ok(Some(_))));

        let broken = dir.join("broken.toml");
        fs::write(&broken, "not = valid = toml").unwrap();
        assert!(matches!(SchedulerConfig::load_if_present(&broken), Err(ConfigError::Parse { .. })));

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn expected_kind_is_an_assertion_helper() {
        assert!(ExpectedKind::Elimination.matches(&GroupKind::Elimination));
        assert!(ExpectedKind::Swiss.matches(&GroupKind::Swiss { num_rounds: 4 }));
        assert!(!ExpectedKind::RoundRobin.matches(&GroupKind::Elimination));
        assert!(!ExpectedKind::Elimination.matches(&GroupKind::Unsupported("MATCHMAKING".to_owned())));
    }
}

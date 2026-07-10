//! Scheduler configuration: the pure shapes (S2) plus the TOML file load and
//! validation that back the `--config` flag (S3).

use std::{
    collections::{BTreeMap, HashSet},
    fs, io,
    path::{Path, PathBuf},
    str::FromStr,
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

# What committing a call marks the set as: "called" (default; players are
# summoned, `p` marks started on arrival) or "in_progress" (chaotic events:
# the caller seats players directly — one less keypress, no no-show alerts).
# call_action = "in_progress"

# How many stations the desk starts with. A single number means one shared
# pool; stations can be added/retired live in the TUI ('s'), and the counts
# here are optional — omit them and the last session's roster carries over.
setups = 4

# Different hardware classes get a count per type instead, and each bracket
# below names its type(s):
#[setups]
#switch = 6
#pokemon = 2

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

# Live-observed start.gg state ints (1=pending 2=in-progress 3=completed
# 6=called). Already the defaults; override only if start.gg changes.
#known_called_state_int = 6
#known_in_progress_state_int = 2

# Same-human links across events, by player id: fill from the preflight
# identity-split report.
#player_aliases = [["1234567", "7654321"]]

# One [[brackets]] block per event at the desk.
[[brackets]]
slug = "tournament/your-tournament/event/your-main-event"
# Preflight warns if the live bracket isn't this shape:
# "elimination" | "round_robin" | "swiss"
expected_kind = "elimination"
# The setup type(s) this event may be called on; omitted = the shared
# default pool. A list means the union of those types' stations:
#setup_type = ["switch", "pokemon"]
# Prior mean bo3 set duration in seconds, blended with observed samples.
#duration_prior_secs = 480

# A second event: mode = "conflict_only" tracks its players as busy but never
# calls or ranks its sets.
#[[brackets]]
#slug = "tournament/your-tournament/event/your-side-event"
#expected_kind = "swiss"
#mode = "conflict_only"
"#;

/// The implicit setup type for brackets that declare none.
pub const DEFAULT_SETUP_TYPE: &str = "default";
/// Stations assumed per referenced type when no count source exists
/// (no config counts, no CLI flag, no defaults file).
pub const FALLBACK_SETUPS_PER_TYPE: u32 = 4;
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

    #[error("`setups = N` implies every bracket is the default type, but {slug:?} declares setup_type {declared:?}")]
    UniformWithTypedBracket { slug: String, declared: String },

    #[error("empty setup type name in {0}")]
    EmptyTypeName(String),

    #[error("sim.duration_noise must be within [0.0, {MAX_DURATION_NOISE}], got {0}")]
    DurationNoiseOutOfRange(f64),
}

/// A physical station at the venue, identified by its position in the TO's
/// numbering (setup 1, setup 2, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SetupId(pub u32);

/// Station counts, either a single number (all stations are the implicit
/// `default` type) or a per-type table. Counts are optional everywhere —
/// persisted/TUI rosters take over when absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SetupCounts {
    Uniform(u32),
    ByType(BTreeMap<String, u32>),
}

/// The `--setups` grammar: a bare count (`8`) or comma-separated per-type
/// counts (`switch=6,pokemon=2`).
impl FromStr for SetupCounts {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let raw = raw.trim();
        if let Ok(n) = raw.parse::<u32>() {
            return Ok(Self::Uniform(n));
        }
        let mut table = BTreeMap::new();
        for part in raw.split(',') {
            let Some((name, count)) = part.split_once('=') else {
                return Err(format!("expected type=count, got {part:?}"));
            };
            let (name, count) = (name.trim(), count.trim());
            if name.is_empty() {
                return Err(format!("empty type name in {part:?}"));
            }
            let count: u32 = count.parse().map_err(|_| format!("bad count in {part:?}"))?;
            if table.insert(name.to_owned(), count).is_some() {
                return Err(format!("duplicate type {name:?}"));
            }
        }
        Ok(Self::ByType(table))
    }
}

/// A `String | Vec<String>` TOML field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchedulerConfig {
    pub brackets: Vec<BracketConfig>,
    /// What committing a call (Enter / the picker) marks the set as.
    /// Chaotic events skip the called/waiting phase and go straight to
    /// started — no no-show alerts, one less keypress per set.
    #[serde(default)]
    pub call_action: CallAction,
    /// How many stations of each type the desk starts with. `None` defers to
    /// the persisted roster / defaults file / fallback (see `resolve_roster`).
    #[serde(default)]
    pub setups: Option<SetupCounts>,
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
    /// Pinned CALLED state int. start.gg's vocabulary is stable, so the
    /// live-observed value is the default; override only if the platform
    /// changes.
    #[serde(default = "default_called_state_int")]
    pub known_called_state_int: Option<i32>,
    /// Pinned IN_PROGRESS state int (same story as CALLED).
    #[serde(default = "default_in_progress_state_int")]
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
            call_action: CallAction::default(),
            setups: None,
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
            known_called_state_int: default_called_state_int(),
            known_in_progress_state_int: default_in_progress_state_int(),
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

        if let Some(SetupCounts::ByType(table)) = &self.setups {
            if table.keys().any(|t| t.is_empty()) {
                return Err(ConfigError::EmptyTypeName("the [setups] table".to_owned()));
            }
        }

        let mut slugs = HashSet::new();
        for bracket in &self.brackets {
            if !slugs.insert(bracket.slug.as_str()) {
                return Err(ConfigError::DuplicateSlug(bracket.slug.clone()));
            }
            for declared in bracket.setup_types() {
                if declared.is_empty() {
                    return Err(ConfigError::EmptyTypeName(format!("bracket {:?}", bracket.slug)));
                }
                // A single count is only unambiguous when every bracket rides
                // the implicit default type.
                if matches!(self.setups, Some(SetupCounts::Uniform(_))) && declared != DEFAULT_SETUP_TYPE {
                    return Err(ConfigError::UniformWithTypedBracket {
                        slug: bracket.slug.clone(),
                        declared,
                    });
                }
            }
        }

        Ok(())
    }
}

/// Every setup type the brackets reference, in first-reference order.
pub fn referenced_types(config: &SchedulerConfig) -> Vec<String> {
    let mut types = Vec::new();
    for bracket in &config.brackets {
        for declared in bracket.setup_types() {
            if !types.contains(&declared) {
                types.push(declared);
            }
        }
    }
    types
}

/// The startup roster derived from counts: types in first-reference order
/// (count-table-only types after, alphabetically), stations numbered
/// contiguously 1..N.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RosterResolution {
    pub roster: Vec<(SetupId, String)>,
    /// No count source existed; every referenced type got the fallback.
    pub fallback: bool,
    /// Bracket-referenced types with zero stations (addable via the TUI).
    pub zero_station_types: Vec<String>,
}

pub fn resolve_roster(config: &SchedulerConfig) -> RosterResolution {
    let referenced = referenced_types(config);
    let mut ordered = referenced.clone();
    if let Some(SetupCounts::ByType(table)) = &config.setups {
        // BTreeMap iterates sorted, so leftovers land alphabetically.
        let leftovers: Vec<String> = table.keys().filter(|t| !ordered.contains(t)).cloned().collect();
        ordered.extend(leftovers);
    }

    let count_of = |setup_type: &str| match &config.setups {
        None => FALLBACK_SETUPS_PER_TYPE,
        Some(SetupCounts::Uniform(n)) => *n,
        Some(SetupCounts::ByType(table)) => table.get(setup_type).copied().unwrap_or(0),
    };

    let mut roster = Vec::new();
    let mut zero_station_types = Vec::new();
    let mut next = 1;
    for setup_type in &ordered {
        let count = count_of(setup_type);
        if count == 0 && referenced.contains(setup_type) {
            zero_station_types.push(setup_type.clone());
        }
        for _ in 0..count {
            roster.push((SetupId(next), setup_type.clone()));
            next += 1;
        }
    }
    RosterResolution {
        roster,
        fallback: config.setups.is_none(),
        zero_station_types,
    }
}

/// The stations a bracket's types entitle it to, before per-setup overrides
/// (those fold in via `conflict::effective_pool`).
pub fn pool_for_types(setup_types: &[String], roster: &[(SetupId, String)]) -> Vec<SetupId> {
    let mut pool: Vec<SetupId> = roster.iter().filter(|(_, t)| setup_types.contains(t)).map(|(id, _)| *id).collect();
    pool.sort();
    pool
}

/// Writes [`STARTER_TEMPLATE`] to `path` for the user to edit.
pub fn write_starter_template(path: &Path) -> io::Result<()> {
    fs::write(path, STARTER_TEMPLATE)
}

/// One scheduled bracket (a start.gg event).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
    /// The setup type(s) this bracket may be called on (a list means the
    /// union of those types' stations). Omitted = the implicit default type.
    #[serde(default)]
    pub setup_type: Option<OneOrMany>,
    /// The start.gg videogame name (`--init-tournament` fills it in). Keys
    /// the character-roster cache: rosters are per game, not per tournament,
    /// so a brand-new event of a known game reports with the right cast even
    /// when its roster fetch fails.
    #[serde(default)]
    pub videogame: Option<String>,
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
            setup_type: None,
            videogame: None,
        }
    }

    /// What the roster cache files this bracket under: the videogame when
    /// known (shared across tournaments), else the event slug.
    pub fn roster_cache_key(&self) -> &str {
        self.videogame.as_deref().unwrap_or(&self.slug)
    }

    pub fn id(&self) -> BracketId {
        BracketId(self.slug.clone())
    }

    pub fn setup_types(&self) -> Vec<String> {
        match &self.setup_type {
            None => vec![DEFAULT_SETUP_TYPE.to_owned()],
            Some(OneOrMany::One(t)) => vec![t.clone()],
            Some(OneOrMany::Many(types)) => types.clone(),
        }
    }
}

/// How much scheduling a bracket gets.
/// The board status + mutation a committed call applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallAction {
    /// Players are summoned; `p` marks the set started when they arrive.
    #[default]
    Called,
    /// Straight to started (chaotic events: callers seat players directly).
    InProgress,
}

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
#[serde(deny_unknown_fields)]
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
    /// CPU ceiling for the forward sims: when stations × incomplete sets
    /// exceeds this, the recompute skips its projections sim and rollout
    /// evaluations stay off — greedy rankings only. (One rollout evaluation
    /// on a 50-station × ~280-set world measured 20s+ of solid CPU.)
    /// 0 = unlimited.
    #[serde(default = "default_sim_world_ceiling")]
    pub world_ceiling: u64,
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
            world_ceiling: default_sim_world_ceiling(),
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

fn default_sim_world_ceiling() -> u64 {
    5_000
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

// start.gg's live-observed vocabulary (1=pending 2=in-progress 3=completed
// 6=called) — platform-wide, not per-tournament.
fn default_called_state_int() -> Option<i32> {
    Some(6)
}

fn default_in_progress_state_int() -> Option<i32> {
    Some(2)
}

fn default_rest_sim_horizon_secs() -> u64 {
    DEFAULT_REST_SIM_HORIZON_SECS
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, env, fs, path::PathBuf, process};

    use serde_json::json;

    use super::{
        pool_for_types, referenced_types, resolve_roster, write_starter_template, BracketConfig, BracketMode, CallAction, ConfigError,
        ExpectedKind, OneOrMany, SchedulerConfig, SetupCounts, SetupId, DEFAULT_DURATION_PRIOR_SECS, DEFAULT_PER_PAGE,
        DEFAULT_POLL_INTERVAL_SECS, DEFAULT_REST_SIM_HORIZON_SECS, FALLBACK_SETUPS_PER_TYPE, STARTER_TEMPLATE,
    };
    use crate::model::GroupKind;

    #[test]
    fn unknown_or_misplaced_keys_fail_loudly() {
        // The classic TOML trap: a top-level key written after a
        // [[brackets]] header belongs to that bracket. Loud beats silent.
        let misplaced = r#"
setups = 2

[[brackets]]
slug = "tournament/t/event/melee"
call_action = "in_progress"
"#;
        let err = toml::from_str::<SchedulerConfig>(misplaced).unwrap_err();
        assert!(err.to_string().contains("call_action"), "{err}");

        let typo = "setups = 2\ncall_actoin = \"in_progress\"\n";
        assert!(toml::from_str::<SchedulerConfig>(typo).is_err());
    }

    #[test]
    fn call_action_parses_top_level() {
        let good = r#"
call_action = "in_progress"
setups = 2

[[brackets]]
slug = "tournament/t/event/melee"
"#;
        let config: SchedulerConfig = toml::from_str(good).unwrap();
        assert_eq!(config.call_action, CallAction::InProgress);
        assert!(config.validate().is_ok());
    }

    fn valid_config() -> SchedulerConfig {
        SchedulerConfig {
            brackets: vec![BracketConfig::new("tournament/t/event/melee")],
            setups: Some(SetupCounts::Uniform(2)),
            ..SchedulerConfig::default()
        }
    }

    fn typed(slug: &str, types: &[&str]) -> BracketConfig {
        let setup_type = match types {
            [one] => OneOrMany::One((*one).to_owned()),
            many => OneOrMany::Many(many.iter().map(|t| (*t).to_owned()).collect()),
        };
        BracketConfig {
            setup_type: Some(setup_type),
            ..BracketConfig::new(slug)
        }
    }

    fn counts(table: &[(&str, u32)]) -> Option<SetupCounts> {
        Some(SetupCounts::ByType(
            table.iter().map(|(t, n)| ((*t).to_owned(), *n)).collect::<BTreeMap<_, _>>(),
        ))
    }

    #[test]
    fn sparse_config_fills_defaults() {
        let config: SchedulerConfig = serde_json::from_value(json!({
            "brackets": [{ "slug": "tournament/t/event/melee", "expected_kind": null, "start_at_override": null }],
        }))
        .unwrap();

        let bracket = &config.brackets[0];
        assert_eq!(bracket.mode, BracketMode::Full);
        assert_eq!(bracket.duration_prior_secs, DEFAULT_DURATION_PRIOR_SECS);
        assert_eq!(bracket.setup_types(), vec!["default"]);
        assert_eq!(config.setups, None);
        assert_eq!(config.rest_window_secs, 0);
        assert!(!config.escalate_unpinned_state_deviation);
        assert_eq!(config.sim.rest_sim_horizon_secs, DEFAULT_REST_SIM_HORIZON_SECS);
        assert_eq!(config.poll_interval_secs, DEFAULT_POLL_INTERVAL_SECS);
        assert_eq!(config.per_page, DEFAULT_PER_PAGE);
        assert!(!config.advisor_only);
        assert_eq!(config.known_called_state_int, Some(6), "live vocabulary is the default");
        assert_eq!(config.known_in_progress_state_int, Some(2));
    }

    #[test]
    fn default_impl_matches_serde_defaults() {
        let sparse: SchedulerConfig = serde_json::from_value(json!({ "brackets": [] })).unwrap();
        let manual = SchedulerConfig::default();
        assert_eq!(sparse.poll_interval_secs, manual.poll_interval_secs);
        assert_eq!(sparse.no_show_secs, manual.no_show_secs);
        assert_eq!(sparse.stale_warn_polls, manual.stale_warn_polls);
        assert_eq!(sparse.per_page, manual.per_page);
        assert_eq!(sparse.setups, manual.setups);
    }

    #[test]
    fn full_toml_round_trip() {
        let toml_src = r#"
            rest_window_secs = 240
            poll_interval_secs = 20
            advisor_only = true
            known_called_state_int = 6
            known_in_progress_state_int = 2
            known_benign_state_ints = [1]
            tournament_slug = "tournament/french-bread-rumble-100"
            token_file = "/tmp/token"
            player_aliases = [["111", "222"]]

            [setups]
            switch = 6
            pokemon = 2

            [[brackets]]
            slug = "tournament/french-bread-rumble-100/event/melee-singles"
            expected_kind = "elimination"
            setup_type = ["switch", "pokemon"]
            duration_prior_secs = 600

            [[brackets]]
            slug = "tournament/french-bread-rumble-100/event/pokemon-champions-4v4-double-battle"
            expected_kind = "swiss"
            setup_type = "pokemon"
            mode = "conflict_only"
        "#;
        let config: SchedulerConfig = toml::from_str(toml_src).unwrap();
        config.validate().unwrap();

        assert_eq!(config.setups, counts(&[("switch", 6), ("pokemon", 2)]));
        assert_eq!(config.poll_interval_secs, 20);
        assert!(config.advisor_only);
        assert_eq!(config.known_called_state_int, Some(6));
        assert_eq!(config.token_file, Some(PathBuf::from("/tmp/token")));
        assert_eq!(config.brackets[0].expected_kind, Some(ExpectedKind::Elimination));
        assert_eq!(config.brackets[0].duration_prior_secs, 600);
        assert_eq!(config.brackets[0].setup_types(), vec!["switch", "pokemon"]);
        assert_eq!(config.brackets[1].mode, BracketMode::ConflictOnly);
        assert_eq!(config.brackets[1].setup_types(), vec!["pokemon"]);
    }

    #[test]
    fn uniform_counts_parse_from_a_single_int() {
        let config: SchedulerConfig = toml::from_str(
            r#"
            setups = 8

            [[brackets]]
            slug = "tournament/t/event/melee"
        "#,
        )
        .unwrap();
        config.validate().unwrap();
        assert_eq!(config.setups, Some(SetupCounts::Uniform(8)));
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
    fn validate_rejects_uniform_counts_with_a_typed_bracket() {
        let mut config = valid_config();
        config.brackets[0].setup_type = Some(OneOrMany::One("pokemon".to_owned()));
        assert!(matches!(config.validate(), Err(ConfigError::UniformWithTypedBracket { .. })));

        // The same declaration under a per-type table is fine.
        config.setups = counts(&[("pokemon", 2)]);
        config.validate().unwrap();
    }

    #[test]
    fn validate_rejects_empty_type_names() {
        let mut config = valid_config();
        config.setups = counts(&[("", 2)]);
        assert!(matches!(config.validate(), Err(ConfigError::EmptyTypeName(_))));

        let mut config = valid_config();
        config.setups = counts(&[("switch", 2)]);
        config.brackets[0].setup_type = Some(OneOrMany::Many(vec!["switch".to_owned(), String::new()]));
        assert!(matches!(config.validate(), Err(ConfigError::EmptyTypeName(_))));
    }

    #[test]
    fn roster_numbers_types_by_first_reference_then_leftovers_alphabetically() {
        let config = SchedulerConfig {
            brackets: vec![
                typed("tournament/t/event/melee", &["switch", "pokemon"]),
                typed("tournament/t/event/pokemon", &["pokemon"]),
            ],
            setups: counts(&[("switch", 2), ("pokemon", 1), ("arcade", 1)]),
            ..SchedulerConfig::default()
        };

        assert_eq!(referenced_types(&config), vec!["switch", "pokemon"]);
        let resolution = resolve_roster(&config);
        assert_eq!(
            resolution.roster,
            vec![
                (SetupId(1), "switch".to_owned()),
                (SetupId(2), "switch".to_owned()),
                (SetupId(3), "pokemon".to_owned()),
                (SetupId(4), "arcade".to_owned()),
            ],
            "first-reference order, then the unreferenced leftover"
        );
        assert!(!resolution.fallback);
        assert!(resolution.zero_station_types.is_empty());
    }

    #[test]
    fn roster_flags_zero_station_referenced_types() {
        let config = SchedulerConfig {
            brackets: vec![
                typed("tournament/t/event/melee", &["switch"]),
                typed("tournament/t/event/pokemon", &["pokemon"]),
            ],
            setups: counts(&[("switch", 2)]),
            ..SchedulerConfig::default()
        };
        let resolution = resolve_roster(&config);
        assert_eq!(resolution.roster.len(), 2);
        assert_eq!(resolution.zero_station_types, vec!["pokemon"]);
        assert!(!resolution.fallback);
    }

    #[test]
    fn roster_without_counts_falls_back_per_referenced_type() {
        let config = SchedulerConfig {
            brackets: vec![
                typed("tournament/t/event/melee", &["switch"]),
                BracketConfig::new("tournament/t/event/side"),
            ],
            setups: None,
            ..SchedulerConfig::default()
        };
        let resolution = resolve_roster(&config);
        assert!(resolution.fallback);
        assert_eq!(resolution.roster.len(), 2 * FALLBACK_SETUPS_PER_TYPE as usize);
        assert_eq!(resolution.roster[0], (SetupId(1), "switch".to_owned()));
        let default_ids = pool_for_types(&["default".to_owned()], &resolution.roster);
        assert_eq!(default_ids, vec![SetupId(5), SetupId(6), SetupId(7), SetupId(8)], "contiguous");
    }

    #[test]
    fn pool_for_types_unions_listed_types() {
        let roster = vec![
            (SetupId(1), "switch".to_owned()),
            (SetupId(2), "switch".to_owned()),
            (SetupId(3), "pokemon".to_owned()),
        ];
        assert_eq!(
            pool_for_types(&["switch".to_owned(), "pokemon".to_owned()], &roster),
            vec![SetupId(1), SetupId(2), SetupId(3)]
        );
        assert_eq!(pool_for_types(&["pokemon".to_owned()], &roster), vec![SetupId(3)]);
        assert!(pool_for_types(&["arcade".to_owned()], &roster).is_empty());
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

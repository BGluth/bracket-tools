//! CLI surface: argument parsing, token resolution, and live-source
//! construction.

use std::{
    env, fs,
    path::{Path, PathBuf},
    str::FromStr,
};

use bracket_tools_startgg::{
    types::{GGRestToken, GGRestTokenParseError},
    GGProvider,
};
use clap::{ArgGroup, Parser};
use directories::ProjectDirs;
use thiserror::Error;

use crate::{
    config::{SchedulerConfig, SetupCounts},
    set_source::StartggSource,
};

pub const STARTGG_TOKEN_ENV: &str = "STARTGG_TOKEN";
pub const DEFAULT_TOKEN_PATH: &str = "~/work/tokens/scraper_gg.token";
pub const CONFIG_FILE: &str = "scheduler.toml";

#[derive(Debug, Parser)]
#[command(name = "scheduler", about = "Multi-bracket calling tool for the TO desk")]
#[command(group = ArgGroup::new("offline").args(["simulate", "synth"]))]
pub struct Cli {
    /// Path to the TOML config file. Defaults to ./scheduler.toml when
    /// present, else the XDG config dir. A missing config writes a starter
    /// template (live mode) or derives one (offline modes).
    #[arg(long)]
    pub config: Option<PathBuf>,

    /// Replay a fixture directory instead of hitting live start.gg.
    #[arg(long, value_name = "DIR")]
    pub simulate: Option<PathBuf>,

    /// Build a synthetic tournament instead of hitting live start.gg:
    /// comma-separated kind:entrants entries (de|se|rr|swiss; swiss takes an
    /// optional :rounds), e.g. `de:32,rr:8`.
    #[arg(long, value_name = "SPEC")]
    pub synth: Option<String>,

    /// Rehearsal mode for --simulate/--synth: script the offline world
    /// forward and play it back at FACTOR x real time (1 = live pace).
    /// Without it, the offline world is served statically.
    #[arg(long, value_name = "FACTOR", requires = "offline")]
    pub pace: Option<f64>,

    /// Auto-play the offline world headlessly: the sim makes every call
    /// itself and writes an ASCII replay + decision log instead of running
    /// the TUI.
    #[arg(long, requires = "offline", conflicts_with = "pace")]
    pub autoplay: bool,

    /// Where --autoplay writes the replay.
    #[arg(long, value_name = "FILE", default_value = "scheduler-replay.txt")]
    pub replay_out: PathBuf,

    /// Play back a replay file written by --autoplay (auto-advancing; space
    /// pauses, arrow keys step back/forward, q quits).
    #[arg(long, value_name = "FILE", conflicts_with_all = ["simulate", "synth", "autoplay", "pace", "preflight_only"])]
    pub replay: Option<PathBuf>,

    /// Frame cadence for --replay playback, in milliseconds.
    #[arg(long, value_name = "MS", default_value_t = 400)]
    pub frame_ms: u64,

    /// Seeded duration noise for --autoplay/--pace: every simulated set gets
    /// a fixed multiplier in 1 ± FRAC (0.25 = ±25%). Overrides the config's
    /// `sim.duration_noise`; default off.
    #[arg(long, value_name = "FRAC", requires = "offline")]
    pub noise: Option<f64>,

    /// Seed for --noise: the same seed replays the identical run, a new one
    /// rolls a different world. Overrides `sim.noise_seed`.
    #[arg(long, value_name = "SEED", requires = "offline")]
    pub noise_seed: Option<u64>,

    /// Station counts for this run: a single number (`8`) or per-type counts
    /// (`switch=6,pokemon=2`). Explicit operator intent — beats the config's
    /// counts AND any persisted roster. Live and offline.
    #[arg(long, value_name = "COUNTS", value_parser = SetupCounts::from_str)]
    pub setups: Option<SetupCounts>,

    /// Disable the capture journal. TODO(S4): the journal itself lands with
    /// persistence; the flag is parsed now so scripts stay stable.
    #[arg(long)]
    pub no_capture: bool,

    /// start.gg API token. Resolution order: this flag, then the
    /// STARTGG_TOKEN environment variable, then the config's `token_file`,
    /// then the default token path.
    #[arg(short = 't', long)]
    pub token: Option<String>,

    /// Never arm writes, regardless of what the admin probe finds.
    #[arg(long)]
    pub advisor_only: bool,

    /// Run the startup preflight, print its report, and exit.
    #[arg(long)]
    pub preflight_only: bool,

    /// Generate a per-tournament config from a start.gg tournament URL (or
    /// `tournament/<slug>`): lists its events, seeds each event's setup
    /// types from the global game-setups.toml mapping, and writes
    /// ./scheduler.toml (or --config's path) for review. Exits after writing.
    #[arg(
        long,
        value_name = "URL_OR_SLUG",
        conflicts_with_all = ["simulate", "synth", "autoplay", "pace", "replay", "preflight_only"]
    )]
    pub init_tournament: Option<String>,
}

impl Cli {
    /// True when the session runs against a fixture world (`--simulate` or
    /// `--synth`) rather than live start.gg.
    pub fn offline(&self) -> bool {
        self.simulate.is_some() || self.synth.is_some()
    }

    /// Where the config lives: the explicit flag, else `./scheduler.toml`
    /// when present (venue-local), else the XDG config dir.
    pub fn config_path(&self) -> PathBuf {
        if let Some(path) = &self.config {
            return path.clone();
        }
        let local = PathBuf::from(CONFIG_FILE);
        if local.exists() {
            return local;
        }
        match project_dirs() {
            Some(dirs) => dirs.config_dir().join(CONFIG_FILE),
            None => local,
        }
    }
}

/// XDG data dir for default state/snapshot files; the working directory
/// stands in when no home exists.
pub fn default_data_dir() -> PathBuf {
    match project_dirs() {
        Some(dirs) => dirs.data_dir().to_path_buf(),
        None => PathBuf::from("."),
    }
}

/// XDG config dir (scheduler.toml's default home, and the global
/// game-setups mapping); the working directory stands in when no home exists.
pub fn default_config_dir() -> PathBuf {
    match project_dirs() {
        Some(dirs) => dirs.config_dir().to_path_buf(),
        None => PathBuf::from("."),
    }
}

fn project_dirs() -> Option<ProjectDirs> {
    ProjectDirs::from("", "", "bracket-tools")
}

#[derive(Debug, Error)]
pub enum TokenError {
    #[error("failed to read token file {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("invalid token from {origin}: {source}")]
    Invalid {
        origin: String,
        #[source]
        source: GGRestTokenParseError,
    },
}

/// Resolves the API token from the highest-precedence source available.
pub fn resolve_token(cli_token: Option<&str>, config: &SchedulerConfig) -> Result<GGRestToken, TokenError> {
    let env_token = env::var(STARTGG_TOKEN_ENV).ok();
    resolve_token_from(cli_token, env_token.as_deref(), config.token_file.as_deref())
}

/// Builds the live start.gg source. The builder's timeout and burst defaults
/// are already tuned for the scheduler's polling profile.
pub fn build_live_source(token: GGRestToken) -> Result<StartggSource, reqwest::Error> {
    Ok(StartggSource::new(GGProvider::builder(token).build()?))
}

fn resolve_token_from(cli: Option<&str>, env: Option<&str>, token_file: Option<&Path>) -> Result<GGRestToken, TokenError> {
    if let Some(raw) = cli {
        return parse_token(raw, "the --token flag");
    }
    if let Some(raw) = env {
        return parse_token(raw, "the STARTGG_TOKEN environment variable");
    }

    let path = expand_home(token_file.unwrap_or(Path::new(DEFAULT_TOKEN_PATH)));
    let raw = fs::read_to_string(&path).map_err(|source| TokenError::Read {
        path: path.clone(),
        source,
    })?;
    parse_token(&raw, &format!("token file {}", path.display()))
}

fn parse_token(raw: &str, origin: &str) -> Result<GGRestToken, TokenError> {
    GGRestToken::from_str(raw).map_err(|source| TokenError::Invalid {
        origin: origin.to_owned(),
        source,
    })
}

/// Expands a leading `~` to `$HOME`; any other path passes through untouched.
fn expand_home(path: &Path) -> PathBuf {
    let Ok(stripped) = path.strip_prefix("~") else {
        return path.to_owned();
    };
    match env::var_os("HOME") {
        Some(home) => PathBuf::from(home).join(stripped),
        None => path.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        env, fs,
        path::{Path, PathBuf},
    };

    use clap::Parser;

    use super::{expand_home, resolve_token_from, Cli, TokenError};
    use crate::config::SetupCounts;

    #[test]
    fn cli_parses_all_flags() {
        let cli = Cli::try_parse_from([
            "scheduler",
            "--config",
            "fbr.toml",
            "--simulate",
            "captures/",
            "--pace",
            "8",
            "--advisor-only",
            "--preflight-only",
            "-t",
            "abc123",
        ])
        .unwrap();

        assert_eq!(cli.config, Some(PathBuf::from("fbr.toml")));
        assert_eq!(cli.config_path(), PathBuf::from("fbr.toml"));
        assert_eq!(cli.simulate, Some(PathBuf::from("captures/")));
        assert_eq!(cli.pace, Some(8.0));
        assert!(cli.offline());
        assert!(cli.advisor_only);
        assert!(cli.preflight_only);
        assert_eq!(cli.token.as_deref(), Some("abc123"));
        assert!(!cli.no_capture);
    }

    #[test]
    fn pace_requires_an_offline_world() {
        assert!(Cli::try_parse_from(["scheduler", "--pace", "8"]).is_err());
        assert!(Cli::try_parse_from(["scheduler", "--synth", "de:8", "--pace", "8"]).is_ok());
        assert!(Cli::try_parse_from(["scheduler", "--simulate", "caps/", "--pace", "8"]).is_ok());
    }

    #[test]
    fn noise_requires_an_offline_world() {
        assert!(Cli::try_parse_from(["scheduler", "--noise", "0.25"]).is_err());
        assert!(Cli::try_parse_from(["scheduler", "--noise-seed", "7"]).is_err());
        let cli = Cli::try_parse_from(["scheduler", "--synth", "de:8", "--noise", "0.25", "--noise-seed", "7"]).unwrap();
        assert_eq!(cli.noise, Some(0.25));
        assert_eq!(cli.noise_seed, Some(7));
    }

    #[test]
    fn simulate_and_synth_are_mutually_exclusive() {
        assert!(Cli::try_parse_from(["scheduler", "--simulate", "caps/", "--synth", "de:8"]).is_err());
    }

    #[test]
    fn setups_flag_parses_both_grammars() {
        let cli = Cli::try_parse_from(["scheduler", "--setups", "8"]).unwrap();
        assert_eq!(cli.setups, Some(SetupCounts::Uniform(8)));

        let cli = Cli::try_parse_from(["scheduler", "--setups", "switch=6, pokemon=2"]).unwrap();
        let expected: BTreeMap<String, u32> = [("switch".to_owned(), 6), ("pokemon".to_owned(), 2)].into();
        assert_eq!(cli.setups, Some(SetupCounts::ByType(expected)));

        for bad in ["switch=6,switch=2", "=3", "switch=", "switch", "switch=x"] {
            assert!(
                Cli::try_parse_from(["scheduler", "--setups", bad]).is_err(),
                "{bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn cli_defaults() {
        let cli = Cli::try_parse_from(["scheduler"]).unwrap();
        assert_eq!(cli.config, None);
        assert!(cli.simulate.is_none());
        assert!(cli.synth.is_none());
        assert!(cli.pace.is_none());
        assert!(!cli.offline());
        assert!(!cli.advisor_only);
        assert!(!cli.preflight_only);
    }

    #[test]
    fn flag_beats_env_beats_file() {
        let token = resolve_token_from(Some("from-flag"), Some("from-env"), None).unwrap();
        assert_eq!(token.as_bearer_value(), "Bearer from-flag");

        let token = resolve_token_from(None, Some("from-env"), Some(Path::new("/nonexistent"))).unwrap();
        assert_eq!(token.as_bearer_value(), "Bearer from-env");
    }

    #[test]
    fn file_fallback_reads_and_trims() {
        let path = env::temp_dir().join(format!("scheduler-token-test-{}", std::process::id()));
        fs::write(&path, "file-token-value\n").unwrap();

        let token = resolve_token_from(None, None, Some(&path)).unwrap();
        assert_eq!(token.as_bearer_value(), "Bearer file-token-value");

        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn missing_file_is_a_read_error() {
        let result = resolve_token_from(None, None, Some(Path::new("/nonexistent/token")));
        assert!(matches!(result, Err(TokenError::Read { .. })));
    }

    #[test]
    fn invalid_token_reports_origin() {
        let err = resolve_token_from(Some("bad token with spaces"), None, None).unwrap_err();
        let TokenError::Invalid { origin, .. } = err else {
            panic!("expected Invalid, got {err:?}");
        };
        assert!(origin.contains("--token"));
    }

    #[test]
    fn expand_home_only_touches_tilde_paths() {
        let home = env::var_os("HOME").expect("HOME set in tests");
        assert_eq!(expand_home(Path::new("~/x/y")), PathBuf::from(home).join("x/y"));
        assert_eq!(expand_home(Path::new("/abs/path")), PathBuf::from("/abs/path"));
        assert_eq!(expand_home(Path::new("rel/path")), PathBuf::from("rel/path"));
    }
}

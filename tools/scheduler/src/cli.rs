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
use clap::Parser;
use thiserror::Error;

use crate::{config::SchedulerConfig, set_source::StartggSource};

pub const STARTGG_TOKEN_ENV: &str = "STARTGG_TOKEN";
pub const DEFAULT_TOKEN_PATH: &str = "~/work/tokens/scraper_gg.token";

#[derive(Debug, Parser)]
#[command(name = "scheduler", about = "Multi-bracket calling tool for the TO desk")]
pub struct Cli {
    /// Path to the TOML config file.
    #[arg(long, default_value = "scheduler.toml")]
    pub config: PathBuf,

    /// Replay a fixture directory instead of hitting live start.gg.
    #[arg(long, value_name = "DIR")]
    pub simulate: Option<PathBuf>,

    /// Rehearsal mode for --simulate: script the captured world forward and
    /// play it back at FACTOR x real time (1 = live pace). Without it,
    /// --simulate serves the captures statically.
    #[arg(long, value_name = "FACTOR", requires = "simulate")]
    pub pace: Option<f64>,

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
        env, fs,
        path::{Path, PathBuf},
    };

    use clap::Parser;

    use super::{expand_home, resolve_token_from, Cli, TokenError};

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

        assert_eq!(cli.config, PathBuf::from("fbr.toml"));
        assert_eq!(cli.simulate, Some(PathBuf::from("captures/")));
        assert_eq!(cli.pace, Some(8.0));
        assert!(cli.advisor_only);
        assert!(cli.preflight_only);
        assert_eq!(cli.token.as_deref(), Some("abc123"));
        assert!(!cli.no_capture);
    }

    #[test]
    fn pace_requires_simulate() {
        assert!(Cli::try_parse_from(["scheduler", "--pace", "8"]).is_err());
    }

    #[test]
    fn cli_defaults() {
        let cli = Cli::try_parse_from(["scheduler"]).unwrap();
        assert_eq!(cli.config, PathBuf::from("scheduler.toml"));
        assert!(cli.simulate.is_none());
        assert!(cli.pace.is_none());
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

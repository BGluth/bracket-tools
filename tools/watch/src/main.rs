//! `gg-watch` — sweeps start.gg for the tournaments in a region, reports
//! what appeared, changed, or vanished since the last sweep, and publishes
//! JSON and iCalendar feeds of the current window. One-shot: run it on a
//! timer. Config: `~/.config/bracket-tools/watch.toml` (see the README).

mod config;
mod diff;
mod feed;
mod files;
mod ics;
mod snapshot;
#[cfg(test)]
mod test_support;

use std::{
    env,
    path::{Path, PathBuf},
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, bail, Context, Result};
use bracket_tools_cache::null_storage::NullStorage;
use bracket_tools_startgg::{types::GGRestToken, GGProvider, TournamentSummary};
use chrono::{DateTime, Local};
use clap::{Parser, Subcommand};

use crate::{
    config::{default_config_path, series_name, Config, Series},
    diff::{diff, time, Change},
    files::{expand_home, write_atomic},
    snapshot::Snapshot,
};

type Provider = GGProvider<NullStorage>;

const PAGE_SIZE: i32 = 50;
const SECS_PER_DAY: i64 = 24 * 3600;
const TOKEN_FALLBACK_PATHS: [&str; 2] = ["~/work/tokens/scraper_gg.token", "~/work/tokens/admin_gg.token"];

#[derive(Parser)]
#[command(
    name = "gg-watch",
    about = "Watch a region on start.gg for new and changed tournaments; publish feeds"
)]
struct Cli {
    /// Config file (default: ~/.config/bracket-tools/watch.toml)
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// start.gg token file. Fallbacks: $STARTGG_TOKEN, the config's
    /// token_file, then ~/work/tokens/scraper_gg.token
    #[arg(long, global = true)]
    token_file: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Sweep, report changes since the last tick, write the feeds, save the snapshot
    Tick {
        /// Sweep and report only; write nothing
        #[arg(long)]
        dry_run: bool,
    },
    /// Show the last snapshot
    Status,
    /// Print the config, snapshot, and feed paths
    Paths,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let config_path = cli.config.clone().unwrap_or_else(default_config_path);

    match cli.command {
        Command::Paths => {
            let config = Config::load(&config_path).ok();
            print_paths(&config_path, config.as_ref());
            Ok(())
        }
        Command::Status => run_status(&Config::load(&config_path)?),
        Command::Tick { dry_run } => {
            let config = Config::load(&config_path)?;
            let token = resolve_token(cli.token_file.as_deref(), config.token_file.as_deref())?;
            let provider = GGProvider::builder(token).page_size(PAGE_SIZE).build()?;
            run_tick(&provider, &config, dry_run).await
        }
    }
}

async fn run_tick(provider: &Provider, config: &Config, dry_run: bool) -> Result<()> {
    let now = unix_now();
    let window_start = now - i64::from(config.history_days) * SECS_PER_DAY;
    let swept = provider
        .fetch_tournaments(&config.filter(window_start))
        .await
        .context("sweeping start.gg")?;
    let current = Snapshot::from_sweep(now, swept);
    let snapshot_path = config.snapshot_path();
    let previous = Snapshot::load(&snapshot_path)?;
    refuse_suspicious_empty_sweep(previous.as_ref(), &current)?;

    match &previous {
        None => println!(
            "first run: recording {} tournaments in the window without reporting them as new",
            current.tournaments.len()
        ),
        Some(prev) => report(&diff(prev, &current, window_start), &config.series),
    }
    if dry_run {
        println!("dry run: nothing written");
        return Ok(());
    }

    write_feeds(config, &current, now)?;
    current.save(&snapshot_path)?;
    Ok(())
}

/// A sweep that comes back empty against a populated snapshot is far more
/// likely an API hiccup than every tournament vanishing at once; reporting it
/// would spray removals and clear the feeds.
fn refuse_suspicious_empty_sweep(previous: Option<&Snapshot>, current: &Snapshot) -> Result<()> {
    if let Some(prev) = previous {
        if !prev.tournaments.is_empty() && current.tournaments.is_empty() {
            bail!(
                "the sweep returned nothing while the last snapshot held {} tournaments; refusing to treat that as {} removals — nothing written",
                prev.tournaments.len(),
                prev.tournaments.len()
            );
        }
    }
    Ok(())
}

fn report(changes: &[Change], series: &[Series]) {
    if changes.is_empty() {
        println!("no changes");
        return;
    }
    for change in changes {
        match change {
            Change::Appeared(t) => println!("+ {}", describe(t, series)),
            Change::Changed { after, fields } => {
                println!("~ {}", describe(after, series));
                for field in fields {
                    println!("    {}: {} -> {}", field.field, field.before, field.after);
                }
            }
            Change::Gone(t) => println!("- {}  (no longer listed)", describe(t, series)),
        }
    }
}

fn write_feeds(config: &Config, snapshot: &Snapshot, now: i64) -> Result<()> {
    let json_path = config.json_path();
    let feed = feed::build(snapshot, &config.series, now);
    write_atomic(&json_path, feed::render(&feed)?.as_bytes())?;

    let ics_path = config.ics_path();
    let calendar = ics::render(snapshot, &config.series, config.calendar_name(), now);
    write_atomic(&ics_path, calendar.as_bytes())?;

    println!("wrote {} and {}", json_path.display(), ics_path.display());
    Ok(())
}

fn run_status(config: &Config) -> Result<()> {
    let Some(snapshot) = Snapshot::load(&config.snapshot_path())? else {
        println!("no snapshot yet: run `gg-watch tick`");
        return Ok(());
    };
    println!(
        "snapshot from {}: {} tournaments",
        time(Some(snapshot.taken_at)),
        snapshot.tournaments.len()
    );
    let now = unix_now();
    for t in snapshot.by_start() {
        let marker = if t.start_at.is_some_and(|s| s < now) { "past" } else { "upcoming" };
        println!("  {marker:<8} {}", describe(t, &config.series));
    }
    Ok(())
}

fn print_paths(config_path: &Path, config: Option<&Config>) {
    println!("config    {}", config_path.display());
    match config {
        Some(config) => {
            println!("snapshot  {}", config.snapshot_path().display());
            println!("json      {}", config.json_path().display());
            println!("ics       {}", config.ics_path().display());
        }
        None => println!("(config missing or invalid; feed paths depend on it)"),
    }
}

fn describe(t: &TournamentSummary, series: &[Series]) -> String {
    let mut parts = vec![local_time(t.start_at), t.name.clone().unwrap_or_else(|| t.slug.clone())];
    if let Some(name) = series_name(series, t) {
        parts.push(format!("[{name}]"));
    }
    if let Some(city) = &t.city {
        parts.push(city.clone());
    }
    let games = t.games();
    if !games.is_empty() {
        parts.push(games.join("/"));
    }
    parts.push(t.url());
    parts.join("  ")
}

fn local_time(unix_secs: Option<i64>) -> String {
    unix_secs.and_then(|secs| DateTime::from_timestamp(secs, 0)).map_or_else(
        || "(undated)".to_string(),
        |dt| dt.with_timezone(&Local).format("%a %Y-%m-%d %H:%M").to_string(),
    )
}

fn unix_now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

fn resolve_token(flag: Option<&Path>, from_config: Option<&Path>) -> Result<GGRestToken> {
    if let Some(path) = flag {
        return Ok(GGRestToken::from_file(path)?);
    }
    if let Ok(raw) = env::var("STARTGG_TOKEN") {
        return GGRestToken::from_str(raw.trim()).map_err(|e| anyhow!("invalid STARTGG_TOKEN: {e}"));
    }
    if let Some(path) = from_config {
        return Ok(GGRestToken::from_file(&expand_home(path))?);
    }
    for candidate in TOKEN_FALLBACK_PATHS {
        let path = expand_home(Path::new(candidate));
        if path.exists() {
            return Ok(GGRestToken::from_file(&path)?);
        }
    }
    bail!(
        "no start.gg token: pass --token-file, set STARTGG_TOKEN, set token_file in the config, or place one at {}",
        TOKEN_FALLBACK_PATHS.join(" or ")
    );
}

//! `gg-admin` — start.gg tournament-admin desk tool (prototype).
//!
//! What the public API allows (and what it doesn't):
//! - `roster`: list a tournament's registered participants; `--unpaid` applies start.gg's server-side unpaid filter. The API exposes no way
//!   to *change* payment state — marking paid stays in the start.gg admin UI.
//! - `add`: register a start.gg user into events of a tournament (e.g. a redemption bracket) via the on-behalf-of token flow. Needs a
//!   tournament-admin token.
//! - `find`: fuzzy-search a player across a saved pool of tournaments (there is no global player search in the public API), ranked by match
//!   quality, then recency, then attendance. The pool lives in `~/.config/bracket-tools/find-pool.toml`, grows automatically with every
//!   tournament the tool touches, and `pool scan` adds a whole series (same owner, same slug stem) in one go. Past tournaments' rosters are
//!   cached on disk so repeated `find` runs stay off the network.

mod fuzzy;
mod store;

use std::{
    cmp::Reverse,
    collections::BTreeMap,
    env,
    io::{self, Write},
    path::{Path, PathBuf},
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, bail, Context, Result};
use bracket_tools_cache::null_storage::NullStorage;
use bracket_tools_startgg::{types::GGRestToken, AdminEvent, AdminParticipant, AdminTournament, GGProvider, StartGgId, TournamentSummary};
use chrono::DateTime;
use clap::{Parser, Subcommand};

use crate::{
    fuzzy::{best_tier, MatchTier},
    store::CachedRoster,
};

type Provider = GGProvider<NullStorage>;

/// Participant pages stay small: each node drags user + events subtrees along,
/// and start.gg rejects queries it deems too complex.
const ROSTER_PAGE_SIZE: i32 = 50;
const TOKEN_FALLBACK_PATHS: [&str; 2] = ["~/work/tokens/admin_gg.token", "~/work/tokens/scraper_gg.token"];
const FIND_RESULT_CAP: usize = 15;
/// A tournament this far in the past no longer gains registrations, so its
/// cached roster is served without a refetch.
const ROSTER_FROZEN_AFTER_SECS: i64 = 2 * 24 * 3600;

#[derive(Parser)]
#[command(name = "gg-admin", version, about = "start.gg tournament-admin desk tool (prototype)")]
struct Cli {
    /// File containing the start.gg API token (admin rights needed for `add`
    /// and the --unpaid filter). Fallbacks: $STARTGG_TOKEN, then
    /// ~/work/tokens/admin_gg.token, then ~/work/tokens/scraper_gg.token.
    #[arg(long, global = true)]
    token_file: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List a tournament's registered participants (admin roster view)
    Roster {
        /// Tournament: slug, `tournament/slug`, or a start.gg URL
        tournament: String,
        /// Only participants start.gg reports as unpaid (server-side filter)
        #[arg(long)]
        unpaid: bool,
        /// Only participants registered in this event (name, slug, or numeric id)
        #[arg(long)]
        event: Option<String>,
    },
    /// Add a player to events of a tournament (e.g. a redemption bracket)
    Add {
        /// Tournament: slug, `tournament/slug`, or a start.gg URL
        tournament: String,
        /// Who to add — fuzzy tag/prefix search over the tournament's own
        /// participants (or a numeric user/participant id)
        query: Option<String>,
        /// Target event (repeatable; name, slug, or numeric id)
        #[arg(long, required = true)]
        event: Vec<String>,
        /// Skip the search and use this start.gg user id directly (e.g. from `find`)
        #[arg(long)]
        user_id: Option<u64>,
        /// Don't ask for confirmation
        #[arg(long)]
        yes: bool,
    },
    /// Fuzzy-find a player across the saved pool of tournaments
    Find {
        /// Tag, prefix, or numeric user id to search for
        query: String,
        /// Extra tournaments beyond the saved pool (repeatable; remembered)
        #[arg(long = "tournament")]
        tournaments: Vec<String>,
        /// Ignore cached rosters and refetch everything live
        #[arg(long)]
        refresh: bool,
    },
    /// Manage the saved search pool that `find` uses by default
    Pool {
        #[command(subcommand)]
        action: PoolAction,
    },
}

#[derive(Subcommand)]
enum PoolAction {
    /// Show the saved pool
    List,
    /// Add tournaments to the pool
    Add {
        #[arg(required = true)]
        tournaments: Vec<String>,
    },
    /// Remove tournaments from the pool
    Remove {
        #[arg(required = true)]
        tournaments: Vec<String>,
    },
    /// Discover a whole series (same owner + same slug stem) and add it
    Scan {
        /// Seed tournament — any tournament of the series
        tournament: Option<String>,
        /// Add all of the owner's tournaments, not just the seed's series
        #[arg(long)]
        all: bool,
        /// Add every tournament your token administers instead (no seed needed)
        #[arg(long, conflicts_with_all = ["tournament", "all"])]
        mine: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let token = resolve_token(cli.token_file.as_deref())?;
    let provider = GGProvider::builder(token).page_size(ROSTER_PAGE_SIZE).build()?;

    match cli.command {
        Command::Roster { tournament, unpaid, event } => run_roster(&provider, &tournament, unpaid, event.as_deref()).await,
        Command::Add {
            tournament,
            query,
            event,
            user_id,
            yes,
        } => run_add(&provider, &tournament, query.as_deref(), &event, user_id, yes).await,
        Command::Find {
            query,
            tournaments,
            refresh,
        } => run_find(&provider, &query, &tournaments, refresh).await,
        Command::Pool { action } => match action {
            PoolAction::List => run_pool_list(),
            PoolAction::Add { tournaments } => run_pool_add(&tournaments),
            PoolAction::Remove { tournaments } => run_pool_remove(&tournaments),
            PoolAction::Scan { tournament, all, mine } => run_pool_scan(&provider, tournament.as_deref(), all, mine).await,
        },
    }
}

async fn run_roster(provider: &Provider, tournament: &str, unpaid: bool, event_filter: Option<&str>) -> Result<()> {
    let slug = normalize_tournament_slug(tournament);
    let (header, participants) = provider.fetch_tournament_admin(&slug, unpaid).await?;
    remember_in_pool(&bare_slug(tournament));
    if !unpaid {
        // Never cache a filtered roster — the cache stands in for the full list.
        cache_roster(&bare_slug(tournament), &header, &participants);
    }

    print_header(&header, &slug);

    let shown: Vec<&AdminParticipant> = match event_filter {
        Some(needle) => {
            let target = resolve_event(&header.events, needle)?;
            println!("filter: {}", event_label(target));
            participants.iter().filter(|p| p.event_ids.contains(&target.id)).collect()
        }
        None => participants.iter().collect(),
    };

    let filter_note = if unpaid { " (unpaid filter)" } else { "" };
    println!("participants{filter_note}: {}", shown.len());

    let tag_width = shown.iter().map(|p| display_tag(p).chars().count()).max().unwrap_or(3).max(3);
    for p in &shown {
        let events = p
            .event_ids
            .iter()
            .map(|id| event_name_by_id(&header, *id))
            .collect::<Vec<_>>()
            .join(", ");
        println!(
            "  {:<tag_width$}  user {:<10}  {}{}{}",
            display_tag(p),
            p.user_id.map_or_else(|| "-".to_string(), |id| id.to_string()),
            if p.checked_in { "[in] " } else { "     " },
            if p.verified { "" } else { "[unverified] " },
            events,
        );
    }

    Ok(())
}

async fn run_add(
    provider: &Provider,
    tournament: &str,
    query: Option<&str>,
    events: &[String],
    user_id_flag: Option<u64>,
    yes: bool,
) -> Result<()> {
    let slug = normalize_tournament_slug(tournament);
    let (header, participants) = provider.fetch_tournament_admin(&slug, false).await?;
    remember_in_pool(&bare_slug(tournament));
    cache_roster(&bare_slug(tournament), &header, &participants);

    let targets = events
        .iter()
        .map(|needle| resolve_event(&header.events, needle))
        .collect::<Result<Vec<_>>>()?;

    let (user_id, label, existing) = match user_id_flag {
        Some(id) => {
            let known = participants.iter().find(|p| p.user_id == Some(id));
            let label = known.map_or_else(|| format!("user {id} (not yet in this tournament)"), display_tag);
            (id, label, known.map(|p| p.event_ids.clone()).unwrap_or_default())
        }
        None => {
            let query = query.ok_or_else(|| anyhow!("give a tag to search for, or pass --user-id"))?;
            let found = resolve_participant(&participants, query)?;
            let user_id = found.user_id.ok_or_else(|| {
                anyhow!(
                    "{} has no start.gg user account; the API can only register real accounts",
                    display_tag(found)
                )
            })?;
            (user_id, display_tag(found), found.event_ids.clone())
        }
    };

    let (already, to_add): (Vec<&AdminEvent>, Vec<&AdminEvent>) = targets.into_iter().partition(|e| existing.contains(&e.id));
    for event in &already {
        println!("already registered in {} — skipping", event_label(event));
    }
    if to_add.is_empty() {
        println!("nothing to do.");
        return Ok(());
    }

    println!("plan: add {label} (user {user_id}) on {}:", header.name.as_deref().unwrap_or(&slug));
    for event in &to_add {
        println!("  + {}", event_label(event));
    }
    if !yes && !confirm("proceed?")? {
        println!("aborted.");
        return Ok(());
    }

    let ids: Vec<StartGgId> = to_add.iter().map(|e| e.id).collect();
    let registered = provider.admin_register_user(user_id, &ids).await?;
    println!(
        "mutation accepted: participant {} ({})",
        registered.id.map_or_else(|| "?".to_string(), |id| id.to_string()),
        registered.gamer_tag.as_deref().unwrap_or("?"),
    );

    verify_registration(provider, &slug, user_id, &ids).await
}

/// Re-fetches the roster so the operator sees the write actually landed.
async fn verify_registration(provider: &Provider, slug: &str, user_id: StartGgId, expected: &[StartGgId]) -> Result<()> {
    let (header, after) = provider.fetch_tournament_admin(slug, false).await?;
    cache_roster(slug.trim_start_matches("tournament/"), &header, &after);

    match after.iter().find(|p| p.user_id == Some(user_id)) {
        Some(p) if expected.iter().all(|id| p.event_ids.contains(id)) => {
            let events = p
                .event_ids
                .iter()
                .map(|id| event_name_by_id(&header, *id))
                .collect::<Vec<_>>()
                .join(", ");
            println!("confirmed: {} is now in: {events}", display_tag(p));
        }
        Some(_) => println!("warning: re-fetch does not yet show all target events (start.gg may lag) — check the attendee page"),
        None => println!("warning: re-fetch does not show the participant yet — check the attendee page"),
    }

    Ok(())
}

/// One person across the whole search pool, merged by user id.
#[derive(Default)]
struct PoolEntry {
    display: String,
    tags: Vec<String>,
    user_id: Option<u64>,
    appearances: usize,
    /// `(start_at, tournament name)` of the most recent appearance.
    last_seen: Option<(i64, String)>,
}

impl PoolEntry {
    fn absorb(&mut self, p: &AdminParticipant, start_at: Option<i64>, tournament: &str) {
        self.appearances += 1;
        self.tags.push(p.gamer_tag.clone());
        if let Some(prefix) = &p.prefix {
            self.tags.push(format!("{prefix} {}", p.gamer_tag));
        }

        let start_at = start_at.unwrap_or(0);
        if self.last_seen.as_ref().is_none_or(|(seen, _)| start_at >= *seen) {
            self.last_seen = Some((start_at, tournament.to_string()));
            self.display = display_tag(p);
        }
        self.user_id = self.user_id.or(p.user_id);
    }
}

async fn run_find(provider: &Provider, query: &str, tournaments: &[String], refresh: bool) -> Result<()> {
    let mut slugs = store::load_pool()?.tournaments;
    for tournament in tournaments {
        let bare = bare_slug(tournament);
        if !slugs.contains(&bare) {
            slugs.push(bare.clone());
        }
        remember_in_pool(&bare);
    }
    if slugs.is_empty() {
        bail!("the search pool is empty: pass --tournament, or seed it with `gg-admin pool add/scan`");
    }

    let mut entries: BTreeMap<String, PoolEntry> = BTreeMap::new();
    let mut skipped = Vec::new();
    for bare in &slugs {
        let (header, participants, from_cache) = match roster_cached_or_live(provider, bare, refresh).await {
            Ok(fetched) => fetched,
            Err(err) => {
                skipped.push(format!("{bare}: {err}"));
                continue;
            }
        };
        let name = header.name.clone().unwrap_or_else(|| bare.clone());
        let source = if from_cache { "cached" } else { "live" };
        println!("pool: {name} — {} participants ({source})", participants.len());

        for p in &participants {
            let key = p
                .user_id
                .map_or_else(|| format!("tag:{}", p.gamer_tag.to_lowercase()), |id| format!("user:{id}"));
            entries.entry(key).or_default().absorb(p, header.start_at, &name);
        }
    }
    for line in &skipped {
        println!("warning: skipped {line}");
    }

    let queried_id = query.parse::<u64>().ok();
    let mut ranked: Vec<(&PoolEntry, MatchTier)> = entries
        .values()
        .filter_map(|entry| {
            let tier = if queried_id.is_some() && queried_id == entry.user_id {
                Some(MatchTier::Exact)
            } else {
                best_tier(query, entry.tags.iter().map(String::as_str))
            };
            tier.map(|t| (entry, t))
        })
        .collect();
    ranked.sort_by(|(a, tier_a), (b, tier_b)| {
        tier_b
            .cmp(tier_a)
            .then_with(|| b.last_seen.cmp(&a.last_seen))
            .then_with(|| b.appearances.cmp(&a.appearances))
            .then_with(|| a.display.cmp(&b.display))
    });

    if ranked.is_empty() {
        println!("no matches for `{query}` in this pool.");
        return Ok(());
    }

    let total = ranked.len();
    ranked.truncate(FIND_RESULT_CAP);
    println!("\nmatches for `{query}` ({} of {total} shown):", ranked.len());

    let tag_width = ranked.iter().map(|(e, _)| e.display.chars().count()).max().unwrap_or(3).max(3);
    for (entry, tier) in &ranked {
        let (last_at, last_name) = entry.last_seen.clone().unwrap_or((0, "?".to_string()));
        let user = entry
            .user_id
            .map_or_else(|| "-  (no account: can't add)".to_string(), |id| id.to_string());
        println!(
            "  {:<7} {:<tag_width$}  user {:<10}  seen {}x  last {} @ {last_name}",
            tier.label(),
            entry.display,
            user,
            entry.appearances,
            format_date(Some(last_at)),
        );
    }
    println!("\nadd with: gg-admin add <tournament> --user-id <id> --event <event>");

    Ok(())
}

fn run_pool_list() -> Result<()> {
    let pool = store::load_pool()?;
    if pool.tournaments.is_empty() {
        println!("pool is empty — seed it with `gg-admin pool add <t>` or `gg-admin pool scan <t>`");
        return Ok(());
    }

    println!(
        "search pool ({} tournaments) — {}",
        pool.tournaments.len(),
        store::pool_path().display()
    );
    for bare in &pool.tournaments {
        match store::load_cached_roster(bare) {
            Some(cached) => println!(
                "  {bare:<44} {}  {:>4} players  {}",
                format_date(cached.header.start_at),
                cached.participants.len(),
                cached.header.name.as_deref().unwrap_or(""),
            ),
            None => println!("  {bare:<44} (roster not yet fetched)"),
        }
    }

    Ok(())
}

fn run_pool_add(tournaments: &[String]) -> Result<()> {
    let mut pool = store::load_pool()?;
    for tournament in tournaments {
        let bare = bare_slug(tournament);
        let note = if pool.add(&bare) { "added" } else { "already present" };
        println!("  {bare}: {note}");
    }
    store::save_pool(&pool)?;
    println!("pool: {} tournaments", pool.tournaments.len());

    Ok(())
}

fn run_pool_remove(tournaments: &[String]) -> Result<()> {
    let mut pool = store::load_pool()?;
    for tournament in tournaments {
        let bare = bare_slug(tournament);
        let note = if pool.remove(&bare) { "removed" } else { "was not in the pool" };
        println!("  {bare}: {note}");
    }
    store::save_pool(&pool)?;
    println!("pool: {} tournaments", pool.tournaments.len());

    Ok(())
}

/// Discovers series siblings (same owner, same slug stem — `fbr-99`/`fbr-100`)
/// or, with `--mine`, every tournament the token administers, and adds the
/// unique ones to the pool.
async fn run_pool_scan(provider: &Provider, seed: Option<&str>, all: bool, mine: bool) -> Result<()> {
    let mut found: Vec<TournamentSummary> = if mine {
        provider.fetch_my_admin_tournaments().await?
    } else {
        let seed = seed.ok_or_else(|| anyhow!("give a seed tournament, or pass --mine"))?;
        let header = provider.fetch_tournament_header(&normalize_tournament_slug(seed)).await?;
        let owner = header
            .owner_id
            .ok_or_else(|| anyhow!("couldn't determine the tournament's owner"))?;
        let mut owned = provider.fetch_tournaments_by_owner(owner).await?;
        if !all {
            let stem = series_stem(&bare_slug(seed)).to_string();
            owned.retain(|t| series_stem(&bare_slug(&t.slug)) == stem);
        }
        owned
    };

    if found.is_empty() {
        println!("no tournaments found (an unlisted/private series may hide from the tournaments query).");
        return Ok(());
    }
    found.sort_by_key(|t| Reverse(t.start_at));

    let mut pool = store::load_pool()?;
    let mut added = 0;
    for tournament in &found {
        let bare = bare_slug(&tournament.slug);
        if pool.add(&bare) {
            added += 1;
            println!(
                "  + {}  {}",
                format_date(tournament.start_at),
                tournament.name.as_deref().unwrap_or(&bare)
            );
        }
    }
    store::save_pool(&pool)?;
    println!(
        "pool: {added} added, {} already present — {} total",
        found.len() - added,
        pool.tournaments.len()
    );

    Ok(())
}

/// Serves a cached roster when the tournament is safely in the past,
/// otherwise fetches live (and refreshes the cache). The bool reports
/// whether the cache answered.
async fn roster_cached_or_live(provider: &Provider, bare: &str, refresh: bool) -> Result<(AdminTournament, Vec<AdminParticipant>, bool)> {
    if !refresh {
        if let Some(cached) = store::load_cached_roster(bare) {
            if cache_usable(cached.header.start_at, now_secs()) {
                return Ok((cached.header, cached.participants, true));
            }
        }
    }

    let (header, participants) = provider.fetch_tournament_admin(&format!("tournament/{bare}"), false).await?;
    cache_roster(bare, &header, &participants);

    Ok((header, participants, false))
}

fn cache_roster(bare: &str, header: &AdminTournament, participants: &[AdminParticipant]) {
    store::save_cached_roster(
        bare,
        &CachedRoster {
            header: header.clone(),
            participants: participants.to_vec(),
            fetched_at: now_secs(),
        },
    );
}

/// A roster is frozen once its tournament is comfortably in the past; recent
/// or upcoming tournaments still gain registrations, so they refetch live.
fn cache_usable(start_at: Option<i64>, now: i64) -> bool {
    start_at.is_some_and(|start| now - start > ROSTER_FROZEN_AFTER_SECS)
}

/// Best-effort pool bookkeeping — must never break the command that ran.
fn remember_in_pool(bare: &str) {
    let Ok(mut pool) = store::load_pool() else { return };
    if pool.add(bare) && store::save_pool(&pool).is_ok() {
        println!("pool: remembered {bare} (see `gg-admin pool list`)");
    }
}

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

fn print_header(header: &AdminTournament, slug: &str) {
    println!(
        "{} ({slug}) — {}",
        header.name.as_deref().unwrap_or("?"),
        format_date(header.start_at),
    );
    println!("events:");
    for event in &header.events {
        println!("  {:>10}  {}", event.id, event_label(event));
    }
}

/// Resolves a user-supplied event needle (numeric id, slug fragment, or name
/// fragment) against the tournament's event list.
fn resolve_event<'a>(events: &'a [AdminEvent], needle: &str) -> Result<&'a AdminEvent> {
    if let Ok(id) = needle.parse::<u64>() {
        if let Some(event) = events.iter().find(|e| e.id == id) {
            return Ok(event);
        }
    }

    let lowered = needle.to_lowercase();
    let matches: Vec<&AdminEvent> = events
        .iter()
        .filter(|e| {
            event_short(&e.slug).to_lowercase().contains(&lowered)
                || e.name.as_deref().unwrap_or_default().to_lowercase().contains(&lowered)
        })
        .collect();

    let exact: Vec<&&AdminEvent> = matches
        .iter()
        .filter(|e| event_short(&e.slug).to_lowercase() == lowered || e.name.as_deref().unwrap_or_default().to_lowercase() == lowered)
        .collect();

    match (matches.as_slice(), exact.as_slice()) {
        ([one], _) => Ok(one),
        (_, [one]) => Ok(one),
        ([], _) => Err(anyhow!("no event matches `{needle}`; events are:\n{}", event_list(events))),
        (many, _) => Err(anyhow!(
            "`{needle}` is ambiguous; it matches:\n{}",
            event_list(&many.iter().map(|e| (*e).clone()).collect::<Vec<_>>())
        )),
    }
}

/// Resolves a fuzzy player query against the tournament's participants. The
/// match must be unique at its best tier; ambiguity is an error listing the
/// contenders (narrow the query or pass --user-id).
fn resolve_participant<'a>(participants: &'a [AdminParticipant], query: &str) -> Result<&'a AdminParticipant> {
    if let Ok(id) = query.parse::<u64>() {
        if let Some(p) = participants.iter().find(|p| p.user_id == Some(id) || p.id == Some(id)) {
            return Ok(p);
        }
    }

    let scored: Vec<(&AdminParticipant, MatchTier)> = participants
        .iter()
        .filter_map(|p| participant_tier(p, query).map(|tier| (p, tier)))
        .collect();
    let best = scored
        .iter()
        .map(|(_, tier)| *tier)
        .max()
        .ok_or_else(|| anyhow!("no participant matches `{query}`"))?;
    let top: Vec<&AdminParticipant> = scored.iter().filter(|(_, tier)| *tier == best).map(|(p, _)| *p).collect();

    match top.as_slice() {
        [one] => Ok(one),
        many => {
            let listing = many
                .iter()
                .map(|p| {
                    format!(
                        "  {}  (user {})",
                        display_tag(p),
                        p.user_id.map_or_else(|| "-".to_string(), |id| id.to_string())
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            Err(anyhow!(
                "`{query}` matches {} participants equally well:\n{listing}\nnarrow the query or pass --user-id",
                many.len()
            ))
        }
    }
}

fn participant_tier(p: &AdminParticipant, query: &str) -> Option<MatchTier> {
    let combined = display_tag(p);
    best_tier(
        query,
        [p.gamer_tag.as_str(), p.prefix.as_deref().unwrap_or_default(), combined.as_str()],
    )
}

fn display_tag(p: &AdminParticipant) -> String {
    match &p.prefix {
        Some(prefix) => format!("{prefix} | {}", p.gamer_tag),
        None => p.gamer_tag.clone(),
    }
}

fn event_label(event: &AdminEvent) -> String {
    match &event.name {
        Some(name) => format!("{name} [{}]", event_short(&event.slug)),
        None => event_short(&event.slug).to_string(),
    }
}

fn event_name_by_id(header: &AdminTournament, id: StartGgId) -> String {
    header
        .events
        .iter()
        .find(|e| e.id == id)
        .map_or_else(|| format!("event {id}"), |e| event_short(&e.slug).to_string())
}

fn event_list(events: &[AdminEvent]) -> String {
    events
        .iter()
        .map(|e| format!("  {:>10}  {}", e.id, event_label(e)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The last path segment of an event slug (`tournament/x/event/y` → `y`).
fn event_short(slug: &str) -> &str {
    slug.rsplit('/').next().unwrap_or(slug)
}

/// Accepts a bare slug, `tournament/foo`, or a full start.gg URL; returns the
/// pinned `tournament/foo` form.
fn normalize_tournament_slug(input: &str) -> String {
    let trimmed = input.trim().trim_end_matches('/');
    if let Some(ix) = trimmed.find("tournament/") {
        let rest = &trimmed[ix + "tournament/".len()..];
        let slug = rest.split('/').next().unwrap_or(rest);
        return format!("tournament/{slug}");
    }
    format!("tournament/{trimmed}")
}

/// The bare slug (`french-bread-rumble-100`) from any accepted tournament form.
fn bare_slug(input: &str) -> String {
    normalize_tournament_slug(input).trim_start_matches("tournament/").to_string()
}

/// The series stem of a bare slug: `french-bread-rumble-100` →
/// `french-bread-rumble`. A slug without a trailing number is its own stem.
fn series_stem(bare: &str) -> &str {
    match bare.rfind('-') {
        Some(ix) if !bare[ix + 1..].is_empty() && bare[ix + 1..].chars().all(|c| c.is_ascii_digit()) => &bare[..ix],
        _ => bare,
    }
}

fn format_date(unix_secs: Option<i64>) -> String {
    unix_secs
        .and_then(|secs| DateTime::from_timestamp(secs, 0))
        .map_or_else(|| "?".to_string(), |dt| dt.format("%Y-%m-%d").to_string())
}

fn confirm(prompt: &str) -> Result<bool> {
    print!("{prompt} [y/N] ");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    Ok(matches!(line.trim().to_lowercase().as_str(), "y" | "yes"))
}

fn resolve_token(flag: Option<&Path>) -> Result<GGRestToken> {
    if let Some(path) = flag {
        return token_from_file(path);
    }
    if let Ok(raw) = env::var("STARTGG_TOKEN") {
        return GGRestToken::from_str(raw.trim()).map_err(|e| anyhow!("invalid STARTGG_TOKEN: {e}"));
    }
    for candidate in TOKEN_FALLBACK_PATHS {
        let path = expand_home(candidate);
        if path.exists() {
            return token_from_file(&path);
        }
    }
    bail!(
        "no start.gg token: pass --token-file, set STARTGG_TOKEN, or place one at {}",
        TOKEN_FALLBACK_PATHS.join(" or ")
    );
}

fn token_from_file(path: &Path) -> Result<GGRestToken> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("reading token file {}", path.display()))?;
    GGRestToken::from_str(raw.trim()).map_err(|e| anyhow!("invalid token in {}: {e}", path.display()))
}

fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(path),
    }
}

#[cfg(test)]
mod tests {
    use bracket_tools_startgg::{AdminEvent, AdminParticipant};

    use super::{
        bare_slug, cache_usable, display_tag, event_short, normalize_tournament_slug, resolve_event, resolve_participant, series_stem,
        ROSTER_FROZEN_AFTER_SECS,
    };

    fn event(id: u64, short: &str, name: &str) -> AdminEvent {
        AdminEvent {
            id,
            slug: format!("tournament/t/event/{short}"),
            name: Some(name.to_string()),
        }
    }

    fn participant(tag: &str, prefix: Option<&str>, user_id: Option<u64>) -> AdminParticipant {
        AdminParticipant {
            id: Some(1),
            gamer_tag: tag.to_string(),
            prefix: prefix.map(str::to_string),
            checked_in: false,
            verified: true,
            user_id,
            user_slug: None,
            event_ids: vec![],
        }
    }

    #[test]
    fn tournament_slug_normalization() {
        for input in [
            "french-bread-rumble-100",
            "tournament/french-bread-rumble-100",
            "https://www.start.gg/tournament/french-bread-rumble-100/details",
            "https://www.start.gg/tournament/french-bread-rumble-100/",
        ] {
            assert_eq!(normalize_tournament_slug(input), "tournament/french-bread-rumble-100");
        }
    }

    #[test]
    fn event_resolution() {
        let events = vec![
            event(11, "ultimate-singles", "Ultimate Singles"),
            event(12, "ultimate-redemption", "Ultimate Redemption"),
            event(13, "melee-singles", "Melee Singles"),
        ];

        assert_eq!(resolve_event(&events, "redemption").unwrap().id, 12);
        assert_eq!(resolve_event(&events, "13").unwrap().id, 13);
        assert_eq!(resolve_event(&events, "Melee Singles").unwrap().id, 13);
        // `singles` hits all three event names ambiguously, exact match on none.
        assert!(resolve_event(&events, "singles").is_err());
        assert!(resolve_event(&events, "doubles").is_err());
    }

    #[test]
    fn participant_resolution() {
        let participants = vec![
            participant("Mango", Some("C9"), Some(501)),
            participant("Mangosteen", None, Some(502)),
            participant("Zelda", None, Some(503)),
        ];

        // Exact tag beats the prefix-tier match on Mangosteen.
        assert_eq!(resolve_participant(&participants, "mango").unwrap().user_id, Some(501));
        assert_eq!(resolve_participant(&participants, "502").unwrap().user_id, Some(502));
        assert_eq!(resolve_participant(&participants, "zel").unwrap().user_id, Some(503));
        assert!(resolve_participant(&participants, "nobody").is_err());

        let twins = vec![participant("Ken", None, Some(601)), participant("Kenny", Some("K"), Some(602))];
        // `ken` is exact on one, prefix on the other — unique at its best tier.
        assert_eq!(resolve_participant(&twins, "ken").unwrap().user_id, Some(601));
        // `ke` is a prefix of both — ambiguous.
        assert!(resolve_participant(&twins, "ke").is_err());
    }

    #[test]
    fn series_stems() {
        assert_eq!(series_stem("french-bread-rumble-100"), "french-bread-rumble");
        assert_eq!(series_stem("fbr-9"), "fbr");
        assert_eq!(series_stem("weekly"), "weekly");
        assert_eq!(series_stem("smash-64-arena"), "smash-64-arena");
        assert_eq!(series_stem("trailing-dash-"), "trailing-dash-");
    }

    #[test]
    fn bare_slugs() {
        assert_eq!(bare_slug("https://www.start.gg/tournament/fbr-100/details"), "fbr-100");
        assert_eq!(bare_slug("tournament/fbr-100"), "fbr-100");
        assert_eq!(bare_slug("fbr-100"), "fbr-100");
    }

    #[test]
    fn cache_freshness_policy() {
        let now = 1_000_000_000;
        // Comfortably past: cached roster is frozen.
        assert!(cache_usable(Some(now - ROSTER_FROZEN_AFTER_SECS - 1), now));
        // Recent, today, or upcoming: always refetch.
        assert!(!cache_usable(Some(now - 3600), now));
        assert!(!cache_usable(Some(now + 3600), now));
        assert!(!cache_usable(None, now));
    }

    #[test]
    fn display_helpers() {
        assert_eq!(display_tag(&participant("Mango", Some("C9"), None)), "C9 | Mango");
        assert_eq!(display_tag(&participant("Zelda", None, None)), "Zelda");
        assert_eq!(event_short("tournament/t/event/ultimate-singles"), "ultimate-singles");
    }
}

//! `gg-admin` — start.gg tournament-admin desk tool (prototype).
//!
//! What the public API allows (and what it doesn't):
//! - `roster`: list a tournament's registered participants; `--unpaid` applies start.gg's server-side unpaid filter. The API exposes no way
//!   to *change* payment state — marking paid stays in the start.gg admin UI.
//! - `add`: register a start.gg user into events of a tournament (e.g. a redemption bracket) via the on-behalf-of token flow. Needs a
//!   tournament-admin token.
//! - `find`: fuzzy-search a player across the rosters of the tournaments you name (there is no global player search in the public API),
//!   ranked by match quality, then recency, then attendance.

mod fuzzy;

use std::{
    collections::BTreeMap,
    env,
    io::{self, Write},
    path::{Path, PathBuf},
    str::FromStr,
};

use anyhow::{anyhow, bail, Context, Result};
use bracket_tools_cache::null_storage::NullStorage;
use bracket_tools_startgg::{types::GGRestToken, AdminEvent, AdminParticipant, AdminTournament, GGProvider, StartGgId};
use chrono::DateTime;
use clap::{Parser, Subcommand};

use crate::fuzzy::{best_tier, MatchTier};

type Provider = GGProvider<NullStorage>;

/// Participant pages stay small: each node drags user + events subtrees along,
/// and start.gg rejects queries it deems too complex.
const ROSTER_PAGE_SIZE: i32 = 50;
const TOKEN_FALLBACK_PATHS: [&str; 2] = ["~/work/tokens/admin_gg.token", "~/work/tokens/scraper_gg.token"];
const FIND_RESULT_CAP: usize = 15;

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
    /// Fuzzy-find a player across the rosters of the given tournaments
    Find {
        /// Tag, prefix, or numeric user id to search for
        query: String,
        /// Tournament to include in the search pool (repeatable)
        #[arg(long = "tournament", required = true)]
        tournaments: Vec<String>,
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
        Command::Find { query, tournaments } => run_find(&provider, &query, &tournaments).await,
    }
}

async fn run_roster(provider: &Provider, tournament: &str, unpaid: bool, event_filter: Option<&str>) -> Result<()> {
    let slug = normalize_tournament_slug(tournament);
    let (header, participants) = provider.fetch_tournament_admin(&slug, unpaid).await?;

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

async fn run_find(provider: &Provider, query: &str, tournaments: &[String]) -> Result<()> {
    let mut pool: BTreeMap<String, PoolEntry> = BTreeMap::new();

    for tournament in tournaments {
        let slug = normalize_tournament_slug(tournament);
        let (header, participants) = provider.fetch_tournament_admin(&slug, false).await?;
        let name = header.name.clone().unwrap_or_else(|| slug.clone());
        println!("pool: {name} — {} participants", participants.len());

        for p in &participants {
            let key = p
                .user_id
                .map_or_else(|| format!("tag:{}", p.gamer_tag.to_lowercase()), |id| format!("user:{id}"));
            pool.entry(key).or_default().absorb(p, header.start_at, &name);
        }
    }

    let queried_id = query.parse::<u64>().ok();
    let mut ranked: Vec<(&PoolEntry, MatchTier)> = pool
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

    use super::{display_tag, event_short, normalize_tournament_slug, resolve_event, resolve_participant};

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
    fn display_helpers() {
        assert_eq!(display_tag(&participant("Mango", Some("C9"), None)), "C9 | Mango");
        assert_eq!(display_tag(&participant("Zelda", None, None)), "Zelda");
        assert_eq!(event_short("tournament/t/event/ultimate-singles"), "ultimate-singles");
    }
}

//! Live start.gg capture + go/no-go smoke for the scheduler SDK spike.
//!
//! Read-only. Per event it (a) fetches the structure and (b) the full set
//! list through `GGProvider` (validating the SDK paths live), then (c)
//! re-captures every page raw — verbatim `GraphQlResponse` envelopes saved to
//! disk, the exact shape a future `FixtureSource` will replay — and grades
//! everything against the S1 smoke checklist into `report.json`/`report.md`.
//!
//! THE go/no-go signal is `empty_slot_sets_with_prereq`: whether
//! `hideEmpty: false` returns unfilled future sets with their prereq fields
//! populated (the bracket DAG's spine). If that stays 0 on a double-elim
//! event, plan B (client-side skeleton synthesis) is selected.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    str::FromStr,
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use bracket_tools_cache::null_storage::NullStorage;
use bracket_tools_startgg::{conversions::extract_event_sets_page, types::GGRestToken, GGProvider, STARTGG_API_URL};
use bracket_tools_startgg_schema::{
    enums::BracketType,
    get_event_structure::{self, GetEventStructure, GetEventStructureVariables},
    get_sets_for_event::{self, GetSetsForEvent, GetSetsForEventVariables},
};
use clap::{ArgGroup, Parser};
use cynic::{GraphQlResponse, Operation, QueryBuilder};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use serde::{de::DeserializeOwned, Serialize};

/// Pause between raw requests: stays well under 80/min even with the
/// provider's governed requests running in the same window.
const RAW_REQUEST_GAP: Duration = Duration::from_millis(750);

#[derive(Parser)]
#[command(about = "Live start.gg capture + go/no-go report for the scheduler SDK spike")]
#[command(group(ArgGroup::new("token_src").required(true)))]
struct SmokeArgs {
    /// start.gg API token value
    #[arg(short = 't', long, group = "token_src")]
    token: Option<String>,

    /// Path to a file containing the start.gg API token
    #[arg(long, group = "token_src")]
    token_file: Option<PathBuf>,

    /// Event slug (repeatable): tournament/<tourney>/event/<event>
    #[arg(long = "event", required_unless_present = "tournaments")]
    events: Vec<String>,

    /// Tournament slug (repeatable): captures EVERY event of
    /// tournament/<tourney>; combines with --event
    #[arg(long = "tournament")]
    tournaments: Vec<String>,

    /// Sets per page (drop to 25 if complexity errors appear)
    #[arg(long, default_value_t = 50)]
    per_page: i32,

    /// Output directory for raw captures + reports
    #[arg(long, default_value = "smoke_out")]
    out: PathBuf,
}

#[derive(Default, Serialize)]
struct SlotFillCounts {
    filled: usize,
    unfilled: usize,
}

#[derive(Default, Serialize)]
struct RrGroupReport {
    sets: usize,
    prereq_edges: usize,
    zero_prereq_edges: bool,
}

#[derive(Default, Serialize)]
struct EventReport {
    event_slug: String,
    tournament_id: Option<String>,
    tournament_slug: Option<String>,
    num_entrants: Option<i32>,
    has_double_elim: bool,
    /// Sets seen in the raw capture (compare with `provider_set_count`).
    set_count: usize,
    /// Sets returned by the provider's paginated fetch.
    provider_set_count: usize,
    started_at_seen: usize,
    completed_at_seen: usize,
    pending_sets_with_null_entrant: usize,
    entrants_missing_participants: usize,
    participants_missing_player: usize,
    /// THE go/no-go: unfilled slots that still carry prereq fields.
    empty_slot_sets_with_prereq: usize,
    sort_stable: bool,
    prereq_types: BTreeSet<String>,
    state_distribution: BTreeMap<i32, SlotFillCounts>,
    sets_missing_state: usize,
    /// True when every raw capture parsed with data and no GraphQL errors.
    full_shape_page_ok: bool,
    graphql_errors: Vec<String>,
    deserialization_failures: Vec<String>,
    raw_errors: Vec<String>,
    provider_errors: Vec<String>,
    rr_phase_groups: BTreeMap<String, RrGroupReport>,
    #[serde(skip)]
    rr_group_ids: BTreeSet<String>,
    non_numeric_set_ids: Vec<String>,
}

impl EventReport {
    fn new(event_slug: &str) -> Self {
        Self {
            event_slug: event_slug.to_string(),
            full_shape_page_ok: true,
            ..Self::default()
        }
    }
}

#[derive(Serialize)]
struct SmokeReport {
    go: bool,
    no_go_reasons: Vec<String>,
    same_tournament_ok: bool,
    raw_requests: usize,
    estimated_provider_requests: usize,
    elapsed_secs: f64,
    requests_per_minute: f64,
    events: Vec<EventReport>,
}

struct Capture<'a> {
    provider: &'a GGProvider<NullStorage>,
    raw_client: &'a reqwest::Client,
    per_page: i32,
    raw_requests: usize,
    provider_requests: usize,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = SmokeArgs::parse();
    let token = resolve_token(&args)?;
    let provider = GGProvider::builder(token.clone()).page_size(args.per_page).build()?;
    let raw_client = build_raw_client(&token)?;
    fs::create_dir_all(&args.out).with_context(|| format!("creating {}", args.out.display()))?;

    let started = Instant::now();
    let event_slugs = expand_events(&provider, &args).await?;
    let mut cx = Capture {
        provider: &provider,
        raw_client: &raw_client,
        per_page: args.per_page,
        raw_requests: 0,
        provider_requests: args.tournaments.len(),
    };

    let mut events = Vec::new();
    for slug in &event_slugs {
        println!("== capturing {slug}");
        let event_dir = args.out.join(slug.replace('/', "_"));
        fs::create_dir_all(&event_dir).with_context(|| format!("creating {}", event_dir.display()))?;
        events.push(capture_event(&mut cx, slug, &event_dir).await);
    }

    let elapsed_secs = started.elapsed().as_secs_f64();
    let total_requests = cx.raw_requests + cx.provider_requests;
    let (go, no_go_reasons) = verdict(&events);
    let report = SmokeReport {
        go,
        no_go_reasons,
        same_tournament_ok: same_tournament_ok(&events),
        raw_requests: cx.raw_requests,
        estimated_provider_requests: cx.provider_requests,
        elapsed_secs,
        requests_per_minute: total_requests as f64 * 60.0 / elapsed_secs.max(f64::EPSILON),
        events,
    };

    fs::write(args.out.join("report.json"), serde_json::to_string_pretty(&report)?)?;
    let md = render_markdown(&report);
    fs::write(args.out.join("report.md"), &md)?;
    println!("\n{md}");

    Ok(())
}

/// The capture list: explicit `--event` slugs plus every event of each
/// `--tournament`, deduplicated in arrival order.
async fn expand_events(provider: &GGProvider<NullStorage>, args: &SmokeArgs) -> Result<Vec<String>> {
    let mut slugs = args.events.clone();
    for tournament in &args.tournaments {
        let events = provider
            .fetch_tournament_events(tournament)
            .await
            .with_context(|| format!("listing events for {tournament}"))?;
        if events.is_empty() {
            bail!("tournament {tournament:?} answered no events (slug typo, or a hidden/unpublished tournament?)");
        }
        println!("== {tournament}: {} event(s)", events.len());
        for event in &events {
            println!("   {} — {}", event.slug, event.name.as_deref().unwrap_or("<unnamed>"));
        }
        slugs.extend(events.into_iter().map(|e| e.slug));
    }

    let mut seen = BTreeSet::new();
    slugs.retain(|slug| seen.insert(slug.clone()));
    Ok(slugs)
}

fn resolve_token(args: &SmokeArgs) -> Result<GGRestToken> {
    let raw = match (&args.token, &args.token_file) {
        (Some(token), _) => token.clone(),
        (None, Some(path)) => fs::read_to_string(path).with_context(|| format!("reading token file {}", path.display()))?,
        (None, None) => unreachable!("clap group guarantees a token source"),
    };

    GGRestToken::from_str(raw.trim()).map_err(|e| anyhow::anyhow!("invalid token: {e}"))
}

fn build_raw_client(token: &GGRestToken) -> Result<reqwest::Client> {
    let mut headers = HeaderMap::new();
    headers.insert(AUTHORIZATION, HeaderValue::from_str(&token.as_bearer_value())?);

    Ok(reqwest::Client::builder()
        .default_headers(headers)
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .build()?)
}

async fn capture_event(cx: &mut Capture<'_>, event_slug: &str, event_dir: &Path) -> EventReport {
    let mut report = EventReport::new(event_slug);

    // (a) Structure through the provider (validates the SDK path live).
    cx.provider_requests += 1;
    match cx.provider.fetch_event_structure(event_slug).await {
        Ok(event) => absorb_structure(&mut report, &event),
        Err(e) => report.provider_errors.push(format!("fetch_event_structure: {e}")),
    }

    // (b) Full set list through the provider (validates the paginator live).
    match cx.provider.fetch_event_sets(event_slug).await {
        Ok(sets) => report.provider_set_count = sets.len(),
        Err(e) => report.provider_errors.push(format!("fetch_event_sets: {e}")),
    }

    // (c) Raw capture: verbatim response envelopes, future fixture material.
    let structure_op = GetEventStructure::build(GetEventStructureVariables { slug: event_slug });
    let _ = raw_fetch_saved(cx, &structure_op, &event_dir.join("structure.json"), &mut report).await;

    let mut sets: Vec<get_sets_for_event::Set> = Vec::new();
    let mut page1_ids: Vec<String> = Vec::new();
    let mut total_pages = 1;
    let mut page = 1;
    while page <= total_pages {
        let operation = build_sets_op(event_slug, page, cx.per_page);
        let path = event_dir.join(format!("sets_page_{page}.json"));
        let Some(data) = raw_fetch_saved(cx, &operation, &path, &mut report).await else {
            break;
        };

        match extract_event_sets_page(&data) {
            Ok(page_data) => {
                if page == 1 {
                    total_pages = page_data.total_pages;
                    page1_ids = set_ids(&page_data.items);
                }
                sets.extend(page_data.items);
            }
            Err(e) => {
                report.full_shape_page_ok = false;
                report.deserialization_failures.push(format!("sets_page_{page}: {e}"));
                break;
            }
        }

        page += 1;
    }

    // The provider's sets fetch ran the same pagination; count it now that
    // the page count is known.
    cx.provider_requests += total_pages.max(1) as usize;

    // Sort-stability probe: page 1 re-fetched must yield the same id sequence.
    let repeat_op = build_sets_op(event_slug, 1, cx.per_page);
    if let Some(data) = raw_fetch_saved(cx, &repeat_op, &event_dir.join("sets_page_1_repeat.json"), &mut report).await {
        if let Ok(page_data) = extract_event_sets_page(&data) {
            report.sort_stable = set_ids(&page_data.items) == page1_ids;
        }
    }

    analyze_sets(&mut report, &sets);

    report
}

fn build_sets_op<'a>(slug: &'a str, page: i32, per_page: i32) -> Operation<GetSetsForEvent, GetSetsForEventVariables<'a>> {
    GetSetsForEvent::build(GetSetsForEventVariables { slug, page, per_page })
}

/// Sleeps the raw-request gap, POSTs the operation, saves the verbatim body,
/// and parses the `GraphQlResponse` envelope, recording every failure mode
/// into the report. Returns the response data when everything parsed.
async fn raw_fetch_saved<ResponseData, Vars>(
    cx: &mut Capture<'_>,
    operation: &Operation<ResponseData, Vars>,
    path: &Path,
    report: &mut EventReport,
) -> Option<ResponseData>
where
    ResponseData: DeserializeOwned,
    Vars: Serialize,
{
    tokio::time::sleep(RAW_REQUEST_GAP).await;
    cx.raw_requests += 1;

    let label = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();

    let text = match cx.raw_client.post(STARTGG_API_URL).json(operation).send().await {
        Ok(response) => match response.text().await {
            Ok(text) => text,
            Err(e) => {
                report.full_shape_page_ok = false;
                report.raw_errors.push(format!("{label}: reading body: {e}"));
                return None;
            }
        },
        Err(e) => {
            report.full_shape_page_ok = false;
            report.raw_errors.push(format!("{label}: POST failed: {e}"));
            return None;
        }
    };

    if let Err(e) = fs::write(path, &text) {
        report.raw_errors.push(format!("{label}: saving capture: {e}"));
    }

    let response: GraphQlResponse<ResponseData> = match serde_json::from_str(&text) {
        Ok(response) => response,
        Err(e) => {
            report.full_shape_page_ok = false;
            report.deserialization_failures.push(format!("{label}: {e}"));
            return None;
        }
    };

    if let Some(errors) = &response.errors {
        if !errors.is_empty() {
            report.full_shape_page_ok = false;
            report
                .graphql_errors
                .extend(errors.iter().map(|e| format!("{label}: {}", e.message)));
        }
    }

    if response.data.is_none() {
        report.full_shape_page_ok = false;
        report.raw_errors.push(format!("{label}: response had no data"));
    }

    response.data
}

fn absorb_structure(report: &mut EventReport, event: &get_event_structure::Event) {
    report.tournament_id = event
        .tournament
        .as_ref()
        .and_then(|t| t.id.as_ref())
        .map(|id| id.inner().to_string());
    report.tournament_slug = event.tournament.as_ref().and_then(|t| t.slug.clone());
    report.num_entrants = event.num_entrants;
    report.has_double_elim = phase_groups(event).any(|pg| pg.bracket_type == Some(BracketType::DoubleElimination));
    report.rr_group_ids = phase_groups(event)
        .filter(|pg| pg.bracket_type == Some(BracketType::RoundRobin))
        .filter_map(|pg| pg.id.as_ref().map(|id| id.inner().to_string()))
        .collect();
}

fn phase_groups(event: &get_event_structure::Event) -> impl Iterator<Item = &get_event_structure::PhaseGroup> {
    event.phase_groups.iter().flatten().flatten()
}

fn set_ids(sets: &[get_sets_for_event::Set]) -> Vec<String> {
    sets.iter().filter_map(|s| s.id.as_ref()).map(|id| id.inner().to_string()).collect()
}

fn analyze_sets(report: &mut EventReport, sets: &[get_sets_for_event::Set]) {
    report.set_count = sets.len();

    for id in &report.rr_group_ids {
        report.rr_phase_groups.entry(id.clone()).or_default();
    }

    for set in sets {
        if let Some(id) = set.id.as_ref() {
            if id.inner().parse::<u64>().is_err() {
                report.non_numeric_set_ids.push(id.inner().to_string());
            }
        }

        if set.started_at.is_some() {
            report.started_at_seen += 1;
        }
        if set.completed_at.is_some() {
            report.completed_at_seen += 1;
        }

        let slots: Vec<&get_sets_for_event::SetSlot> = set.slots.iter().flatten().flatten().collect();
        let filled = !slots.is_empty() && slots.iter().all(|s| s.entrant.is_some());
        let has_empty_slot = slots.iter().any(|s| s.entrant.is_none());

        if has_empty_slot && set.completed_at.is_none() {
            report.pending_sets_with_null_entrant += 1;
        }
        if slots
            .iter()
            .any(|s| s.entrant.is_none() && (s.prereq_id.is_some() || s.prereq_type.is_some()))
        {
            report.empty_slot_sets_with_prereq += 1;
        }

        analyze_slots(report, &slots);

        match set.state {
            Some(state) => {
                let counts = report.state_distribution.entry(state).or_default();
                if filled {
                    counts.filled += 1;
                } else {
                    counts.unfilled += 1;
                }
            }
            None => report.sets_missing_state += 1,
        }

        if let Some(pg_id) = set.phase_group.as_ref().and_then(|pg| pg.id.as_ref()) {
            let pg_id = pg_id.inner().to_string();
            if report.rr_group_ids.contains(&pg_id) {
                let group = report.rr_phase_groups.entry(pg_id).or_default();
                group.sets += 1;
                group.prereq_edges += slots.iter().filter(|s| s.prereq_id.is_some()).count();
            }
        }
    }

    for group in report.rr_phase_groups.values_mut() {
        group.zero_prereq_edges = group.prereq_edges == 0;
    }
}

fn analyze_slots(report: &mut EventReport, slots: &[&get_sets_for_event::SetSlot]) {
    for slot in slots {
        if let Some(prereq_type) = &slot.prereq_type {
            report.prereq_types.insert(prereq_type.clone());
        }

        let Some(entrant) = &slot.entrant else { continue };
        let participants: Vec<_> = entrant.participants.iter().flatten().flatten().collect();

        if participants.is_empty() {
            report.entrants_missing_participants += 1;
        }
        for participant in participants {
            if participant.player.as_ref().and_then(|p| p.id.as_ref()).is_none() {
                report.participants_missing_player += 1;
            }
        }
    }
}

/// GO requires: prereq fields on empty slots of a double-elim event, clean
/// full-shape pages, stable ROUND sort, startedAt observed, zero
/// deserialization failures.
fn verdict(events: &[EventReport]) -> (bool, Vec<String>) {
    let mut reasons = Vec::new();

    if !events.iter().any(|e| e.has_double_elim && e.empty_slot_sets_with_prereq > 0) {
        reasons.push("no double-elim event yielded empty-slot sets with prereq fields (hideEmpty:false disconfirmed)".to_string());
    }
    if !events.iter().all(|e| e.full_shape_page_ok) {
        reasons.push("full-shape page failures (see graphql_errors/raw_errors)".to_string());
    }
    if !events.iter().all(|e| e.sort_stable) {
        reasons.push("ROUND sort order not stable across re-fetch".to_string());
    }
    if events.iter().map(|e| e.started_at_seen).sum::<usize>() == 0 {
        reasons.push("no startedAt timestamps observed (i64 Timestamp unproven)".to_string());
    }
    if events.iter().any(|e| !e.deserialization_failures.is_empty()) {
        reasons.push("deserialization failures".to_string());
    }

    (reasons.is_empty(), reasons)
}

/// Events under the same `tournament/<name>` slug prefix must report the same
/// tournament id.
fn same_tournament_ok(events: &[EventReport]) -> bool {
    let mut ids_by_prefix: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();

    for event in events {
        if let (Some(prefix), Some(id)) = (tournament_prefix(&event.event_slug), event.tournament_id.as_deref()) {
            ids_by_prefix.entry(prefix).or_default().insert(id);
        }
    }

    ids_by_prefix.values().all(|ids| ids.len() <= 1)
}

fn tournament_prefix(event_slug: &str) -> Option<&str> {
    let tourney = event_slug.strip_prefix("tournament/")?.split('/').next()?;
    Some(&event_slug[.."tournament/".len() + tourney.len()])
}

fn render_markdown(report: &SmokeReport) -> String {
    let mut md = String::new();

    let _ = writeln!(md, "# Scheduler SDK smoke report\n");
    let _ = writeln!(md, "## Verdict: **{}**\n", if report.go { "GO" } else { "NO-GO" });
    for reason in &report.no_go_reasons {
        let _ = writeln!(md, "- NO-GO: {reason}");
    }
    let _ = writeln!(
        md,
        "- requests: {} raw + ~{} provider over {:.1}s ({:.1} req/min)",
        report.raw_requests, report.estimated_provider_requests, report.elapsed_secs, report.requests_per_minute
    );
    let _ = writeln!(
        md,
        "- cross-event same-tournament check: {}\n",
        if report.same_tournament_ok { "ok" } else { "MISMATCH" }
    );

    for event in &report.events {
        render_event(&mut md, event);
    }

    md
}

fn render_event(md: &mut String, event: &EventReport) {
    let _ = writeln!(md, "## {}\n", event.event_slug);
    let _ = writeln!(
        md,
        "- tournament: {} ({})",
        event.tournament_id.as_deref().unwrap_or("?"),
        event.tournament_slug.as_deref().unwrap_or("?")
    );
    let _ = writeln!(
        md,
        "- sets: {} raw / {} provider; entrants: {}; double-elim: {}",
        event.set_count,
        event.provider_set_count,
        event.num_entrants.map_or("?".to_string(), |n| n.to_string()),
        event.has_double_elim
    );
    let _ = writeln!(
        md,
        "- **empty-slot sets with prereq: {}** (go/no-go)",
        event.empty_slot_sets_with_prereq
    );
    let _ = writeln!(
        md,
        "- startedAt seen: {}; completedAt seen: {}; sort stable: {}; full-shape ok: {}",
        event.started_at_seen, event.completed_at_seen, event.sort_stable, event.full_shape_page_ok
    );
    let _ = writeln!(
        md,
        "- pending sets w/ null entrant: {}; entrants w/o participants: {}; participants w/o player: {}",
        event.pending_sets_with_null_entrant, event.entrants_missing_participants, event.participants_missing_player
    );
    let _ = writeln!(md, "- prereq types: {:?}", event.prereq_types);
    let _ = writeln!(md, "- non-numeric set ids: {:?}", event.non_numeric_set_ids);

    if !event.state_distribution.is_empty() || event.sets_missing_state > 0 {
        let _ = writeln!(md, "- state distribution (state → filled/unfilled):");
        for (state, counts) in &event.state_distribution {
            let _ = writeln!(md, "    - {state}: {}/{}", counts.filled, counts.unfilled);
        }
        if event.sets_missing_state > 0 {
            let _ = writeln!(md, "    - (missing state): {}", event.sets_missing_state);
        }
    }

    if !event.rr_phase_groups.is_empty() {
        let _ = writeln!(md, "- round-robin phase groups:");
        for (id, group) in &event.rr_phase_groups {
            let _ = writeln!(
                md,
                "    - {id}: {} sets, {} prereq edges{}",
                group.sets,
                group.prereq_edges,
                if group.zero_prereq_edges { " (zero-edge)" } else { "" }
            );
        }
    }

    for (label, errors) in [
        ("provider errors", &event.provider_errors),
        ("raw errors", &event.raw_errors),
        ("graphql errors", &event.graphql_errors),
        ("deserialization failures", &event.deserialization_failures),
    ] {
        if !errors.is_empty() {
            let _ = writeln!(md, "- {label}:");
            for error in errors {
                let _ = writeln!(md, "    - {error}");
            }
        }
    }

    let _ = writeln!(md);
}

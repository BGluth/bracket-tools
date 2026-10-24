//! Bracket-structure admin for `gg-admin`: list, create, reshape and delete
//! phases; rewrite a phase's seeding from an ordered player list; show an
//! event's standings; and fill a redemption bracket from a placement cutoff.
//!
//! What the public API cannot do here: create an event (a redemption bracket
//! is a second event, made in the start.gg admin UI first) or mark a bracket
//! started — both stay in the UI.

use std::iter::once;

use anyhow::{anyhow, bail, Result};
use bracket_tools_startgg::{
    ActivityState, AdminEvent, BracketType, EntrantRef, EventBracket, EventStanding, PhaseInfo, PhaseSeed, PhaseSpec, StartGgId,
};
use clap::ValueEnum;

use crate::{
    confirm, event_label, fetch_tournament,
    fuzzy::{best_tier, MatchTier},
    register_candidates, resolve_event, AddCandidate, Provider,
};

/// The bracket formats the CLI can set on a phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum BracketKind {
    DoubleElimination,
    SingleElimination,
    RoundRobin,
    Swiss,
}

impl From<BracketKind> for BracketType {
    fn from(kind: BracketKind) -> Self {
        match kind {
            BracketKind::DoubleElimination => BracketType::DoubleElimination,
            BracketKind::SingleElimination => BracketType::SingleElimination,
            BracketKind::RoundRobin => BracketType::RoundRobin,
            BracketKind::Swiss => BracketType::Swiss,
        }
    }
}

pub async fn run_phases(provider: &Provider, tournament: &str, event: Option<&str>) -> Result<()> {
    let (slug, header, _participants) = fetch_tournament(provider, tournament).await?;
    let events: Vec<&AdminEvent> = match event {
        Some(needle) => vec![resolve_event(&header.events, needle)?],
        None => header.events.iter().collect(),
    };

    println!("{} ({slug})", header.name.as_deref().unwrap_or("?"));
    for event in events {
        print_bracket(&provider.fetch_event_bracket(event.id).await?);
    }

    Ok(())
}

pub async fn run_phase_create(
    provider: &Provider,
    tournament: &str,
    event: &str,
    name: &str,
    bracket: BracketKind,
    groups: Option<i32>,
) -> Result<()> {
    let (_slug, header, _participants) = fetch_tournament(provider, tournament).await?;
    let event = resolve_event(&header.events, event)?;

    let spec = PhaseSpec {
        name: Some(name.to_string()),
        bracket_type: Some(bracket.into()),
        group_count: groups,
    };
    let created = provider.create_phase(event.id, spec).await?;
    println!(
        "created phase {} on {}: {}",
        created.id,
        event_label(event),
        phase_summary(&created)
    );
    print_bracket(&provider.fetch_event_bracket(event.id).await?);

    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn run_phase_set(
    provider: &Provider,
    tournament: &str,
    event: &str,
    phase: &str,
    name: Option<&str>,
    bracket: Option<BracketKind>,
    groups: Option<i32>,
    yes: bool,
) -> Result<()> {
    if name.is_none() && bracket.is_none() && groups.is_none() {
        bail!("nothing to change: pass --name, --bracket and/or --groups");
    }
    let (_slug, header, _participants) = fetch_tournament(provider, tournament).await?;
    let event = resolve_event(&header.events, event)?;
    let before = provider.fetch_event_bracket(event.id).await?;
    let target = resolve_phase(&before, Some(phase))?;

    println!("plan: phase {} on {} — {}:", target.id, event_label(event), phase_summary(target));
    if let Some(name) = name {
        println!("  name     {} -> {name}", target.name.as_deref().unwrap_or("?"));
    }
    if let Some(bracket) = bracket {
        println!(
            "  bracket  {} -> {}",
            bracket_label(target.bracket_type),
            bracket_label(Some(bracket.into()))
        );
    }
    if let Some(groups) = groups {
        println!("  groups   {} -> {groups}", target.group_count.unwrap_or(0));
    }
    if !yes && !confirm("proceed?")? {
        println!("aborted.");
        return Ok(());
    }

    let spec = PhaseSpec {
        name: name.map(str::to_string),
        bracket_type: bracket.map(Into::into),
        group_count: groups,
    };
    let updated = provider.update_phase(target.id, spec).await?;
    println!("mutation accepted: phase {} is now {}", updated.id, phase_summary(&updated));
    print_bracket(&provider.fetch_event_bracket(event.id).await?);

    Ok(())
}

pub async fn run_phase_delete(provider: &Provider, tournament: &str, event: &str, phase: &str, yes: bool) -> Result<()> {
    let (_slug, header, _participants) = fetch_tournament(provider, tournament).await?;
    let event = resolve_event(&header.events, event)?;
    let before = provider.fetch_event_bracket(event.id).await?;
    let target = resolve_phase(&before, Some(phase))?;

    println!(
        "plan: delete phase {} ({}) from {}",
        target.id,
        phase_summary(target),
        event_label(event)
    );
    if !yes && !confirm("proceed?")? {
        println!("aborted.");
        return Ok(());
    }

    if provider.delete_phase(target.id).await? {
        println!("deleted.");
    } else {
        println!("warning: start.gg reported the phase was not deleted");
    }
    print_bracket(&provider.fetch_event_bracket(event.id).await?);

    Ok(())
}

/// Rewrites a phase's seeding. `order` lists players best-first (tags, entrant
/// names or user ids); entrants left unlisted keep their relative order
/// behind the listed ones, so a partial list seeds the top and leaves the
/// rest alone.
pub async fn run_seed(
    provider: &Provider,
    tournament: &str,
    event: &str,
    phase: Option<&str>,
    order: &[String],
    dry_run: bool,
    yes: bool,
) -> Result<()> {
    if order.is_empty() {
        bail!("give the seed order: tags, entrant names or user ids, best first");
    }
    let (_slug, header, _participants) = fetch_tournament(provider, tournament).await?;
    let event = resolve_event(&header.events, event)?;
    let bracket = provider.fetch_event_bracket(event.id).await?;
    let target = resolve_phase(&bracket, phase)?;
    let seeds = sorted_seeds(provider.fetch_phase_seeds(target.id).await?);
    if seeds.is_empty() {
        bail!(
            "phase {} ({}) has no seeds yet — entrants appear here once registered",
            target.id,
            phase_summary(target)
        );
    }

    let ordered = resolve_seed_order(&seeds, order)?;
    println!(
        "seeding plan for {} / {} ({} seeds):",
        event_label(event),
        phase_summary(target),
        ordered.len()
    );
    for (ix, seed) in ordered.iter().enumerate() {
        let was = seed.seed_num.map_or_else(|| "-".to_string(), |n| n.to_string());
        let note = if ix < order.len() { "" } else { "  (unlisted: kept in order)" };
        println!("  {:>3}  was {was:>3}  {}{note}", ix + 1, seed_label(seed));
    }
    if dry_run {
        println!("dry run: nothing written.");
        return Ok(());
    }
    if !yes && !confirm("proceed?")? {
        println!("aborted.");
        return Ok(());
    }

    let ids: Vec<StartGgId> = ordered.iter().map(|s| s.id).collect();
    let num_seeds = provider.update_phase_seeding(target.id, &ids, true).await?;
    println!(
        "mutation accepted: phase now reports {} seeds",
        num_seeds.map_or_else(|| "?".to_string(), |n| n.to_string())
    );

    let after = sorted_seeds(provider.fetch_phase_seeds(target.id).await?);
    if after.iter().map(|s| s.id).eq(ids.iter().copied()) {
        println!("confirmed: start.gg returns the new order.");
    } else {
        println!("warning: the re-fetched order differs from the plan — check the bracket page:");
        for seed in &after {
            println!("  {:>3}  {}", seed.seed_num.unwrap_or(0), seed_label(seed));
        }
    }

    Ok(())
}

pub async fn run_standings(provider: &Provider, tournament: &str, event: &str) -> Result<()> {
    let (_slug, header, _participants) = fetch_tournament(provider, tournament).await?;
    let event = resolve_event(&header.events, event)?;
    let standings = provider.fetch_event_standings(event.id).await?;

    let finality = if !standings.is_empty() && standings.iter().all(|s| s.is_final) {
        " (final)"
    } else {
        ""
    };
    println!("{} — {} standings{finality}", event_label(event), standings.len());
    for standing in &standings {
        println!("  {:>3}  {}", standing.placement, entrant_label(&standing.entrant));
    }

    Ok(())
}

/// Registers everyone who placed at or below `place` in `from` into `to`.
/// Entrants without a start.gg account (or team entrants) are listed for
/// manual handling rather than guessed at.
pub async fn run_redemption(
    provider: &Provider,
    tournament: &str,
    from: &str,
    to: &str,
    place: i32,
    dry_run: bool,
    yes: bool,
) -> Result<()> {
    let (slug, header, _participants) = fetch_tournament(provider, tournament).await?;
    let source = resolve_event(&header.events, from)?;
    let target = resolve_event(&header.events, to)?;
    if source.id == target.id {
        bail!("--from and --to name the same event");
    }

    let standings = provider.fetch_event_standings(source.id).await?;
    if standings.is_empty() {
        bail!(
            "{} has no standings yet — the bracket must produce placements before a redemption cut can be made",
            event_label(source)
        );
    }
    if standings.iter().any(|s| !s.is_final) {
        println!("warning: some standings are not final yet — placements may still move");
    }

    let (eligible, above): (Vec<&EventStanding>, Vec<&EventStanding>) = standings.iter().partition(|s| s.placement >= place);
    println!(
        "{}: {} entrants placed {} or lower ({} placed higher):",
        event_label(source),
        eligible.len(),
        ordinal(place),
        above.len()
    );
    let mut candidates = Vec::new();
    for standing in &eligible {
        let entrant = &standing.entrant;
        match entrant.user_ids.as_slice() {
            [user_id] => candidates.push(AddCandidate {
                user_id: *user_id,
                label: format!("{} ({})", entrant.name, ordinal(standing.placement)),
            }),
            [] => println!(
                "  skip {:>3}  {} — no start.gg account; register manually",
                standing.placement, entrant.name
            ),
            many => println!(
                "  skip {:>3}  {} — team entrant with {} accounts; add them individually",
                standing.placement,
                entrant.name,
                many.len()
            ),
        }
    }

    register_candidates(provider, &slug, &header, candidates, &[target], yes, dry_run).await
}

/// Resolves a phase needle (numeric id or name fragment) against an event's
/// phases; `None` picks the first phase in play order.
fn resolve_phase<'a>(bracket: &'a EventBracket, needle: Option<&str>) -> Result<&'a PhaseInfo> {
    let Some(needle) = needle else {
        return bracket
            .phases
            .first()
            .ok_or_else(|| anyhow!("{} has no phases", bracket.name.as_deref().unwrap_or("the event")));
    };
    if let Ok(id) = needle.parse::<u64>() {
        if let Some(phase) = bracket.phases.iter().find(|p| p.id == id) {
            return Ok(phase);
        }
    }

    let lowered = needle.to_lowercase();
    let matches: Vec<&PhaseInfo> = bracket
        .phases
        .iter()
        .filter(|p| phase_name(p).to_lowercase().contains(&lowered))
        .collect();
    let exact: Vec<&PhaseInfo> = matches
        .iter()
        .copied()
        .filter(|p| phase_name(p).to_lowercase() == lowered)
        .collect();

    match (matches.as_slice(), exact.as_slice()) {
        ([one], _) => Ok(one),
        (_, [one]) => Ok(one),
        ([], _) => Err(anyhow!(
            "no phase matches `{needle}`; phases are:\n{}",
            phase_list(bracket.phases.iter())
        )),
        (many, _) => Err(anyhow!(
            "`{needle}` is ambiguous; it matches:\n{}",
            phase_list(many.iter().copied())
        )),
    }
}

/// Maps the operator's order onto seeds: each item must pick exactly one
/// seed, no seed twice; the seeds not listed follow in their current order.
fn resolve_seed_order<'a>(seeds: &'a [PhaseSeed], order: &[String]) -> Result<Vec<&'a PhaseSeed>> {
    let mut picked: Vec<&PhaseSeed> = Vec::with_capacity(seeds.len());
    for item in order {
        let seed = resolve_seed(seeds, item)?;
        if picked.iter().any(|p| p.id == seed.id) {
            bail!("`{item}` picks {} a second time", seed_label(seed));
        }
        picked.push(seed);
    }
    let unlisted: Vec<&PhaseSeed> = seeds.iter().filter(|s| !picked.iter().any(|p| p.id == s.id)).collect();
    picked.extend(unlisted);

    Ok(picked)
}

/// Resolves one order item against the seeds: a number matches a user id,
/// entrant id or seed id; anything else is a fuzzy tag/name match that must
/// be unique at its best tier.
fn resolve_seed<'a>(seeds: &'a [PhaseSeed], query: &str) -> Result<&'a PhaseSeed> {
    if let Ok(id) = query.parse::<u64>() {
        let by_id = seeds
            .iter()
            .find(|s| s.id == id || s.entrant.as_ref().is_some_and(|e| e.id == id || e.user_ids.contains(&id)));
        if let Some(seed) = by_id {
            return Ok(seed);
        }
    }

    let scored: Vec<(&PhaseSeed, MatchTier)> = seeds.iter().filter_map(|s| seed_tier(s, query).map(|tier| (s, tier))).collect();
    let best = scored
        .iter()
        .map(|(_, tier)| *tier)
        .max()
        .ok_or_else(|| anyhow!("no seed matches `{query}`; seeds are:\n{}", seed_list(seeds.iter())))?;
    let top: Vec<&PhaseSeed> = scored.iter().filter(|(_, tier)| *tier == best).map(|(s, _)| *s).collect();

    match top.as_slice() {
        [one] => Ok(one),
        many => Err(anyhow!(
            "`{query}` matches {} seeds equally well:\n{}\nnarrow the query or pass a user id",
            many.len(),
            seed_list(many.iter().copied())
        )),
    }
}

fn seed_tier(seed: &PhaseSeed, query: &str) -> Option<MatchTier> {
    let entrant = seed.entrant.as_ref()?;
    let bare_tags = entrant.tags.iter().map(|tag| tag.rsplit(" | ").next().unwrap_or(tag));

    best_tier(
        query,
        once(entrant.name.as_str())
            .chain(entrant.tags.iter().map(String::as_str))
            .chain(bare_tags),
    )
}

fn sorted_seeds(mut seeds: Vec<PhaseSeed>) -> Vec<PhaseSeed> {
    seeds.sort_by_key(|s| s.seed_num.unwrap_or(i32::MAX));
    seeds
}

fn print_bracket(bracket: &EventBracket) {
    println!(
        "\n{} [{}]  entrants {}  state {}",
        bracket.name.as_deref().unwrap_or("?"),
        bracket.id,
        bracket.num_entrants,
        state_label(bracket.state)
    );
    if bracket.phases.is_empty() {
        println!("  (no phases)");
    }
    for phase in &bracket.phases {
        let groups = phase
            .groups
            .iter()
            .map(|g| g.display_identifier.clone().unwrap_or_else(|| g.id.to_string()))
            .collect::<Vec<_>>()
            .join(", ");
        println!(
            "  {:>2}. {:<9} {:<22} {:<18} groups {} [{groups}]  seeds {}  state {}",
            phase.phase_order.unwrap_or(0),
            phase.id,
            phase_name(phase),
            bracket_label(phase.bracket_type),
            phase.group_count.unwrap_or(0),
            phase.num_seeds.unwrap_or(0),
            state_label(phase.state),
        );
    }
}

fn phase_list<'a>(phases: impl Iterator<Item = &'a PhaseInfo>) -> String {
    phases
        .map(|p| format!("  {:>9}  {}", p.id, phase_summary(p)))
        .collect::<Vec<_>>()
        .join("\n")
}

fn seed_list<'a>(seeds: impl Iterator<Item = &'a PhaseSeed>) -> String {
    seeds
        .map(|s| format!("  {:>3}  {}", s.seed_num.unwrap_or(0), seed_label(s)))
        .collect::<Vec<_>>()
        .join("\n")
}

fn phase_name(phase: &PhaseInfo) -> &str {
    phase.name.as_deref().unwrap_or("?")
}

fn phase_summary(phase: &PhaseInfo) -> String {
    format!(
        "{} / {} / {} group(s)",
        phase_name(phase),
        bracket_label(phase.bracket_type),
        phase.group_count.unwrap_or(0)
    )
}

fn seed_label(seed: &PhaseSeed) -> String {
    match &seed.entrant {
        Some(entrant) => entrant_label(entrant),
        None if seed.is_bye => "bye".to_string(),
        None => format!("seed {} (placeholder)", seed.id),
    }
}

fn entrant_label(entrant: &EntrantRef) -> String {
    let users = entrant.user_ids.iter().map(ToString::to_string).collect::<Vec<_>>().join(",");
    let users = if users.is_empty() { "-".to_string() } else { users };

    format!("{}  (user {users})", entrant.name)
}

fn bracket_label(bracket_type: Option<BracketType>) -> &'static str {
    match bracket_type {
        Some(BracketType::DoubleElimination) => "double elimination",
        Some(BracketType::SingleElimination) => "single elimination",
        Some(BracketType::RoundRobin) => "round robin",
        Some(BracketType::Swiss) => "swiss",
        Some(BracketType::Exhibition) => "exhibition",
        Some(BracketType::CustomSchedule) => "custom schedule",
        Some(BracketType::Matchmaking) => "matchmaking",
        Some(BracketType::EliminationRounds) => "elimination rounds",
        Some(BracketType::Race) => "race",
        Some(BracketType::Circuit) => "circuit",
        None => "?",
    }
}

fn state_label(state: Option<ActivityState>) -> &'static str {
    match state {
        Some(ActivityState::Created) => "created",
        Some(ActivityState::Active) => "active",
        Some(ActivityState::Completed) => "completed",
        Some(ActivityState::Ready) => "ready",
        Some(ActivityState::Invalid) => "invalid",
        Some(ActivityState::Called) => "called",
        Some(ActivityState::Queued) => "queued",
        None => "?",
    }
}

fn ordinal(n: i32) -> String {
    let suffix = match (n % 10, n % 100) {
        (1, 11) | (2, 12) | (3, 13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };

    format!("{n}{suffix}")
}

#[cfg(test)]
mod tests {
    use bracket_tools_startgg::{BracketType, EntrantRef, EventBracket, PhaseInfo, PhaseSeed};

    use super::{ordinal, resolve_phase, resolve_seed, resolve_seed_order};

    fn phase(id: u64, name: &str, order: i32) -> PhaseInfo {
        PhaseInfo {
            id,
            name: Some(name.to_string()),
            bracket_type: Some(BracketType::DoubleElimination),
            group_count: Some(1),
            num_seeds: Some(0),
            phase_order: Some(order),
            state: None,
            groups: vec![],
        }
    }

    fn bracket(phases: Vec<PhaseInfo>) -> EventBracket {
        EventBracket {
            id: 1,
            name: Some("Ultimate Singles".to_string()),
            slug: None,
            state: None,
            num_entrants: 0,
            phases,
        }
    }

    fn seed(id: u64, num: i32, name: &str, tags: &[&str], user_id: u64) -> PhaseSeed {
        PhaseSeed {
            id,
            seed_num: Some(num),
            is_bye: false,
            entrant: Some(EntrantRef {
                id: id * 10,
                name: name.to_string(),
                tags: tags.iter().map(|t| t.to_string()).collect(),
                user_ids: vec![user_id],
            }),
        }
    }

    #[test]
    fn phase_resolution() {
        // Phases arrive in play order from the SDK; `None` picks the first.
        let event = bracket(vec![phase(10, "Bracket", 1), phase(20, "Top 8", 2)]);

        assert_eq!(resolve_phase(&event, None).unwrap().id, 10);
        assert_eq!(resolve_phase(&event, Some("20")).unwrap().id, 20);
        assert_eq!(resolve_phase(&event, Some("top")).unwrap().id, 20);
        assert_eq!(resolve_phase(&event, Some("Bracket")).unwrap().id, 10);
        assert!(resolve_phase(&event, Some("pools")).is_err());
        assert!(resolve_phase(&bracket(vec![]), None).is_err());
    }

    #[test]
    fn seed_resolution_by_tag_name_and_id() {
        let seeds = vec![
            seed(1, 1, "McD | French Fries", &["McD | French Fries"], 501),
            seed(2, 2, "Breeze", &["Breeze"], 502),
            seed(3, 3, "FreakBob | GasMoney", &["FreakBob | GasMoney"], 503),
        ];

        assert_eq!(
            resolve_seed(&seeds, "french fries").unwrap().id,
            1,
            "bare tag matches without the prefix"
        );
        assert_eq!(resolve_seed(&seeds, "Breeze").unwrap().id, 2);
        assert_eq!(resolve_seed(&seeds, "503").unwrap().id, 3, "a number matches the user id");
        assert_eq!(resolve_seed(&seeds, "20").unwrap().id, 2, "or the entrant id");
        assert!(resolve_seed(&seeds, "nobody").is_err());
        // `e` is a substring of every entrant — ambiguous.
        assert!(resolve_seed(&seeds, "e").is_err());
    }

    #[test]
    fn seed_order_appends_unlisted_and_rejects_repeats() {
        let seeds = vec![
            seed(1, 1, "Alpha", &["Alpha"], 1),
            seed(2, 2, "Bravo", &["Bravo"], 2),
            seed(3, 3, "Charlie", &["Charlie"], 3),
        ];

        let ordered = resolve_seed_order(&seeds, &["charlie".to_string(), "alpha".to_string()]).unwrap();
        assert_eq!(ordered.iter().map(|s| s.id).collect::<Vec<_>>(), vec![3, 1, 2]);

        assert!(resolve_seed_order(&seeds, &["alpha".to_string(), "1".to_string()]).is_err());
    }

    #[test]
    fn ordinals() {
        assert_eq!(ordinal(1), "1st");
        assert_eq!(ordinal(2), "2nd");
        assert_eq!(ordinal(3), "3rd");
        assert_eq!(ordinal(4), "4th");
        assert_eq!(ordinal(11), "11th");
        assert_eq!(ordinal(12), "12th");
        assert_eq!(ordinal(13), "13th");
        assert_eq!(ordinal(21), "21st");
    }
}

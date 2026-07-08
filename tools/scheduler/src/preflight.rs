//! Startup preflight: fetch every configured event's structure + first
//! snapshot, assert tournament identity and expected bracket kinds, classify
//! failures into the three buckets, and scan for split player identities.
//!
//! Non-interactive by design (overnight/unattended launches): every fork
//! takes the safe default and says so in the report — connectivity failures
//! launch the bracket empty in its configured mode (the poller keeps
//! trying), definitive failures downgrade it to conflict-only.
//!
//! Admin probe: writes arm only when the caller asked for them, every event
//! preflighted clean, AND the tournament's admin list (an admin-only field)
//! contains the token's user. A probe the network won't answer falls back to
//! the fetch-success proxy with a warning; a definitive rejection or a
//! non-admin answer disarms. Advisor-only with no pinned CALLED int
//! additionally arms the soft-busy escalation (remote-call detection would
//! otherwise be blind).

use std::{
    collections::{BTreeSet, HashMap},
    fmt::Write as _,
    time::Duration,
};

use bracket_tools_startgg::{CharacterInfo, StartGgId};
use tokio::time::timeout;

use crate::{
    app::{BracketBootstrap, PollFailure},
    config::{BracketConfig, BracketMode, SchedulerConfig},
    model::{live_sets_from_schema, phase_groups_from_schema, LiveSet, PhaseGroupInfo, PlayerId},
    set_source::SetSource,
};

#[derive(Debug)]
pub struct PreflightReport {
    pub brackets: Vec<BracketPreflight>,
    /// (id, slug) every healthy event agreed on.
    pub tournament: Option<(String, String)>,
    pub identity_splits: Vec<IdentitySplit>,
    pub writes_armed: bool,
    /// How the admin probe decided (rendered in the report).
    pub admin_probe: Option<String>,
    /// Advisor-only with no pinned CALLED int: remote-call detection is
    /// degraded, so unpinned state-int deviations should escalate to
    /// soft-busy. The caller applies this to the running config.
    pub escalate_soft_busy: bool,
    /// Set when launching would be meaningless (identity mismatch, nothing
    /// reachable definitively).
    pub fatal: Option<String>,
}

#[derive(Debug)]
pub struct BracketPreflight {
    pub config: BracketConfig,
    pub outcome: BracketOutcome,
    pub warnings: Vec<String>,
    /// (id, slug) of the owning tournament, when the structure answered —
    /// input to the cross-event identity assertion.
    pub tournament: Option<(String, String)>,
    /// The event's character roster (reporting vocabulary); empty when the
    /// videogame has none or the fetch failed (best-effort).
    pub characters: Vec<CharacterInfo>,
}

#[derive(Debug)]
pub enum BracketOutcome {
    Ready {
        sets: Vec<LiveSet>,
        groups: Vec<PhaseGroupInfo>,
        event_start_at: Option<i64>,
    },
    /// Connectivity trouble: launch empty in the configured mode, no prompt;
    /// the poller keeps trying.
    Offline { groups: Vec<PhaseGroupInfo>, error: String },
    /// Definitive failure (bad slug, permissions): downgraded to
    /// conflict-only so a typo can't silently absorb calls.
    Failed { error: String },
}

/// The same case-folded gamer tag appearing in multiple events with disjoint
/// player-id sets — likely one human the conflict index can't link. The TO
/// adds a `player_aliases` entry if real.
#[derive(Debug, PartialEq, Eq)]
pub struct IdentitySplit {
    pub tag: String,
    pub identities: Vec<(String, BTreeSet<PlayerId>)>,
}

pub async fn preflight<S, F>(
    source: &S,
    config: &SchedulerConfig,
    request_timeout: Duration,
    arm_writes: bool,
    classify: F,
) -> PreflightReport
where
    S: SetSource,
    F: Fn(&S::Error) -> PollFailure,
{
    let mut brackets = Vec::new();
    for bracket_config in &config.brackets {
        brackets.push(preflight_bracket(source, bracket_config, request_timeout, &classify).await);
    }

    let mut fatal = None;
    let tournament = assert_tournament_identity(&brackets, config, &mut fatal);
    let identity_splits = scan_identity_splits(&brackets);

    let any_ready = brackets.iter().any(|b| matches!(b.outcome, BracketOutcome::Ready { .. }));
    if !any_ready && fatal.is_none() {
        fatal = Some("no configured event preflighted successfully".to_owned());
    }
    let all_ready = brackets.iter().all(|b| matches!(b.outcome, BracketOutcome::Ready { .. }));
    // The probe can only further restrict the S3 proxy (requested + every
    // event fetched clean), never arm what the proxy wouldn't.
    let proxy_armed = arm_writes && fatal.is_none() && all_ready;
    let (writes_armed, admin_probe) = if proxy_armed {
        resolve_admin_probe(source, request_timeout, &tournament, &classify).await
    } else {
        (false, None)
    };
    let escalate_soft_busy = !writes_armed && config.known_called_state_int.is_none();

    PreflightReport {
        brackets,
        tournament,
        identity_splits,
        writes_armed,
        admin_probe,
        escalate_soft_busy,
        fatal,
    }
}

/// The writes-armed decision table, given a proxy-armed launch. Connectivity
/// trouble keeps the proxy's answer (permission was never *denied*); a
/// definitive rejection or a non-admin answer disarms.
async fn resolve_admin_probe<S, F>(
    source: &S,
    request_timeout: Duration,
    tournament: &Option<(String, String)>,
    classify: &F,
) -> (bool, Option<String>)
where
    S: SetSource,
    F: Fn(&S::Error) -> PollFailure,
{
    let Some((raw_id, _)) = tournament else {
        return (
            true,
            Some("no tournament id available — armed on the fetch-success proxy".to_owned()),
        );
    };
    let Ok(id) = raw_id.parse::<StartGgId>() else {
        return (
            true,
            Some(format!("non-numeric tournament id {raw_id:?} — armed on the fetch-success proxy")),
        );
    };
    match timeout(request_timeout, source.probe_admin(id)).await {
        Err(_elapsed) => (true, Some("admin probe timed out — armed on the fetch-success proxy".to_owned())),
        Ok(Err(error)) => match classify(&error) {
            PollFailure::Offline | PollFailure::Transient => (
                true,
                Some(format!("admin probe unreachable ({error}) — armed on the fetch-success proxy")),
            ),
            PollFailure::Persistent(msg) => (false, Some(format!("admin probe rejected definitively ({msg}) — advisor-only"))),
        },
        Ok(Ok(result)) => {
            if result.is_admin() {
                (true, Some("token administers this tournament — writes armed".to_owned()))
            } else {
                let why = match (&result.current_user, &result.admins) {
                    (None, _) => "token carries no user identity",
                    (_, None) => "admin list hidden from this token (not an admin)",
                    _ => "token's user is not among the tournament admins",
                };
                (false, Some(format!("{why} — advisor-only")))
            }
        }
    }
}

async fn preflight_bracket<S, F>(source: &S, config: &BracketConfig, request_timeout: Duration, classify: &F) -> BracketPreflight
where
    S: SetSource,
    F: Fn(&S::Error) -> PollFailure,
{
    let mut warnings = Vec::new();

    let structure = match timeout(request_timeout, source.fetch_event_structure(&config.slug)).await {
        Err(_elapsed) => {
            return BracketPreflight {
                config: config.clone(),
                outcome: BracketOutcome::Offline {
                    groups: Vec::new(),
                    error: "structure fetch timed out".to_owned(),
                },
                warnings,
                tournament: None,
                characters: Vec::new(),
            }
        }
        Ok(Err(error)) => {
            let outcome = match classify(&error) {
                PollFailure::Offline | PollFailure::Transient => BracketOutcome::Offline {
                    groups: Vec::new(),
                    error: format!("structure fetch failed: {error}"),
                },
                PollFailure::Persistent(msg) => BracketOutcome::Failed {
                    error: format!("structure fetch failed definitively: {msg}"),
                },
            };
            return BracketPreflight {
                config: config.clone(),
                outcome,
                warnings,
                tournament: None,
                characters: Vec::new(),
            };
        }
        Ok(Ok(structure)) => structure,
    };

    let (groups, group_warnings) = phase_groups_from_schema(&structure);
    warnings.extend(group_warnings.iter().map(|w| format!("{w:?}")));
    if let Some(expected) = config.expected_kind {
        if !groups.iter().any(|g| expected.matches(&g.kind)) {
            warnings.push(format!(
                "expected_kind {expected:?} matches no live phase group ({:?})",
                groups.iter().map(|g| &g.kind).collect::<Vec<_>>()
            ));
        }
    }
    let event_start_at = structure.start_at.map(|ts| ts.0);
    let tournament = structure.tournament.as_ref();
    let tournament_pair = tournament.and_then(|t| Some((t.id.as_ref()?.inner().to_owned(), t.slug.clone().unwrap_or_default())));

    let outcome = match timeout(request_timeout, source.fetch_event_sets(&config.slug)).await {
        Err(_elapsed) => BracketOutcome::Offline {
            groups,
            error: "set fetch timed out".to_owned(),
        },
        Ok(Err(error)) => match classify(&error) {
            PollFailure::Offline | PollFailure::Transient => BracketOutcome::Offline {
                groups,
                error: format!("set fetch failed: {error}"),
            },
            PollFailure::Persistent(msg) => BracketOutcome::Failed {
                error: format!("set fetch failed definitively: {msg}"),
            },
        },
        Ok(Ok(schema_sets)) => {
            let (sets, model_warnings, skipped) = live_sets_from_schema(schema_sets);
            if !skipped.is_empty() {
                warnings.push(format!("{} sets skipped in conversion", skipped.len()));
            }
            for warning in &model_warnings {
                if let crate::model::ModelWarning::UnsupportedGroup { phase_group, raw } = warning {
                    warnings.push(format!("unsupported group {phase_group}: {raw}"));
                }
            }
            BracketOutcome::Ready {
                sets,
                groups,
                event_start_at,
            }
        }
    };

    // Best-effort roster fetch for healthy events (the reporting vocabulary);
    // an event without one still schedules — reporting just skips characters.
    let characters = match &outcome {
        BracketOutcome::Ready { .. } => match timeout(request_timeout, source.fetch_event_characters(&config.slug)).await {
            Ok(Ok(characters)) => characters,
            Ok(Err(error)) => {
                warnings.push(format!("character roster unavailable: {error}"));
                Vec::new()
            }
            Err(_elapsed) => {
                warnings.push("character roster fetch timed out".to_owned());
                Vec::new()
            }
        },
        _ => Vec::new(),
    };

    BracketPreflight {
        config: config.clone(),
        outcome,
        warnings,
        tournament: tournament_pair,
        characters,
    }
}

fn assert_tournament_identity(
    brackets: &[BracketPreflight],
    config: &SchedulerConfig,
    fatal: &mut Option<String>,
) -> Option<(String, String)> {
    let mut agreed: Option<(String, String)> = None;
    for bracket in brackets {
        let Some(pair) = bracket.tournament.clone() else { continue };
        match &agreed {
            None => agreed = Some(pair),
            Some(existing) if existing.0 == pair.0 => {}
            Some(existing) => {
                *fatal = Some(format!(
                    "tournament identity mismatch: {} is under {:?} but earlier events are under {:?}",
                    bracket.config.slug, pair.1, existing.1
                ));
            }
        }
    }
    if let (Some(expected), Some((_, actual_slug))) = (&config.tournament_slug, &agreed) {
        // start.gg returns short tournament slugs; config may carry the
        // fully-qualified form. Compare suffix-insensitively.
        let matches = expected == actual_slug
            || expected.strip_prefix("tournament/") == Some(actual_slug)
            || actual_slug.strip_prefix("tournament/") == Some(expected.as_str());
        if !matches {
            *fatal = Some(format!(
                "configured tournament_slug {expected:?} does not match the live tournament {actual_slug:?}"
            ));
        }
    }
    agreed
}

/// Case-folded gamer tags appearing in several events whose player-id sets
/// are pairwise disjoint (nothing links them as one human).
fn scan_identity_splits(brackets: &[BracketPreflight]) -> Vec<IdentitySplit> {
    let mut by_tag: HashMap<String, Vec<(String, BTreeSet<PlayerId>)>> = HashMap::new();
    for bracket in brackets {
        let BracketOutcome::Ready { sets, .. } = &bracket.outcome else {
            continue;
        };
        let mut per_event: HashMap<String, BTreeSet<PlayerId>> = HashMap::new();
        for occupant in sets.iter().flat_map(LiveSet::occupants) {
            if occupant.player_ids.is_empty() {
                continue;
            }
            per_event
                .entry(occupant.display_name.to_lowercase())
                .or_default()
                .extend(occupant.player_ids.iter().cloned());
        }
        for (tag, ids) in per_event {
            by_tag.entry(tag).or_default().push((bracket.config.slug.clone(), ids));
        }
    }

    let mut splits: Vec<IdentitySplit> = by_tag
        .into_iter()
        .filter(|(_, identities)| {
            identities.len() >= 2 && {
                let all: BTreeSet<_> = identities.iter().flat_map(|(_, ids)| ids.iter()).collect();
                // Pairwise disjoint == no id shared == total size is the sum.
                all.len() == identities.iter().map(|(_, ids)| ids.len()).sum::<usize>()
            }
        })
        .map(|(tag, identities)| IdentitySplit { tag, identities })
        .collect();
    splits.sort_by(|a, b| a.tag.cmp(&b.tag));
    splits
}

impl BracketPreflight {
    /// Effective mode after preflight: definitive failures downgrade to
    /// conflict-only.
    pub fn effective_mode(&self) -> BracketMode {
        match self.outcome {
            BracketOutcome::Failed { .. } => BracketMode::ConflictOnly,
            _ => self.config.mode,
        }
    }
}

impl PreflightReport {
    /// Consumes the report into per-bracket bootstraps for [`crate::app::AppState::new`].
    pub fn into_bootstraps(self) -> Vec<BracketBootstrap> {
        self.brackets
            .into_iter()
            .map(|bracket| {
                let mode = bracket.effective_mode();
                let config = bracket.config;
                let (sets, groups, event_start_at) = match bracket.outcome {
                    BracketOutcome::Ready {
                        sets,
                        groups,
                        event_start_at,
                    } => (sets, groups, event_start_at),
                    BracketOutcome::Offline { groups, .. } => (Vec::new(), groups, None),
                    BracketOutcome::Failed { .. } => (Vec::new(), Vec::new(), None),
                };
                BracketBootstrap {
                    id: config.id(),
                    sets,
                    groups,
                    mode,
                    start_at: config.start_at_override.or(event_start_at),
                    pool: config.pool.clone(),
                    duration_prior_secs: config.duration_prior_secs,
                    prior_weight: config.prior_weight,
                    characters: bracket.characters,
                }
            })
            .collect()
    }

    /// Human-readable report (printed before the TUI takes the screen, and
    /// the whole output of `--preflight-only`).
    pub fn render(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "scheduler preflight");
        let _ = writeln!(out, "===================");
        match &self.tournament {
            Some((id, slug)) => {
                let _ = writeln!(out, "tournament: {slug} (id {id})");
            }
            None => {
                let _ = writeln!(out, "tournament: <no event answered>");
            }
        }
        let _ = writeln!(
            out,
            "writes: {}",
            if self.writes_armed { "ARMED" } else { "advisor-only (disarmed)" }
        );
        if let Some(probe) = &self.admin_probe {
            let _ = writeln!(out, "admin probe: {probe}");
        }
        if self.escalate_soft_busy {
            let _ = writeln!(
                out,
                "WARNING: writes disabled and no CALLED int pinned — remote-call detection degraded; \
                 unpinned state-int deviations will escalate to soft-busy (run the web-UI capture and pin known_called_state_int)"
            );
        }
        for bracket in &self.brackets {
            let status = match &bracket.outcome {
                BracketOutcome::Ready { sets, groups, .. } => {
                    format!("ready — {} sets, {} groups", sets.len(), groups.len())
                }
                BracketOutcome::Offline { error, .. } => format!("OFFLINE (launching empty, poller retries) — {error}"),
                BracketOutcome::Failed { error } => format!("FAILED (downgraded to conflict-only) — {error}"),
            };
            let _ = writeln!(out, "  {}: {status}", bracket.config.slug);
            for warning in &bracket.warnings {
                let _ = writeln!(out, "    warn: {warning}");
            }
        }
        for split in &self.identity_splits {
            let events = split
                .identities
                .iter()
                .map(|(slug, ids)| format!("{slug} {ids:?}"))
                .collect::<Vec<_>>()
                .join("; ");
            let _ = writeln!(
                out,
                "identity split: tag {:?} has unlinked ids across events — consider a player_aliases entry ({events})",
                split.tag
            );
        }
        if let Some(fatal) = &self.fatal {
            let _ = writeln!(out, "FATAL: {fatal}");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use std::{slice::from_ref, time::Duration};

    use super::{preflight, BracketOutcome};
    use crate::{
        app::PollFailure,
        config::{BracketConfig, BracketMode, ExpectedKind, SchedulerConfig, SetupId},
        fixture_source::{FixtureError, FixtureSource},
        synth::{make_de_bracket, make_de_bracket_with, SynthPlayer},
    };

    const TIMEOUT: Duration = Duration::from_millis(100);
    const ULTIMATE: &str = "tournament/fbr/event/ultimate";
    const MELEE: &str = "tournament/fbr/event/melee";

    fn classify(_: &FixtureError) -> PollFailure {
        PollFailure::Persistent("unknown event".to_owned())
    }

    fn config_for(slugs: &[&str]) -> SchedulerConfig {
        SchedulerConfig {
            setups: vec![SetupId(1)],
            brackets: slugs
                .iter()
                .map(|slug| BracketConfig {
                    pool: vec![SetupId(1)],
                    ..BracketConfig::new(*slug)
                })
                .collect(),
            ..SchedulerConfig::default()
        }
    }

    fn two_event_source() -> FixtureSource {
        let ultimate = make_de_bracket(1001, 8);
        let melee = make_de_bracket(2001, 4);
        let mut source = FixtureSource::new();
        source.add_synth_event(ULTIMATE, from_ref(&ultimate.info), vec![ultimate.sets]);
        source.add_synth_event(MELEE, from_ref(&melee.info), vec![melee.sets]);
        source
    }

    #[tokio::test]
    async fn healthy_events_preflight_ready_and_arm() {
        let source = two_event_source();
        let config = config_for(&[ULTIMATE, MELEE]);

        let report = preflight(&source, &config, TIMEOUT, true, classify).await;

        assert!(report.fatal.is_none(), "{report:?}");
        assert!(report.writes_armed);
        assert!(report.brackets.iter().all(|b| matches!(b.outcome, BracketOutcome::Ready { .. })));
        let (_, slug) = report.tournament.as_ref().expect("identity agreed");
        assert_eq!(slug, "tournament/fbr");

        let boots = report.into_bootstraps();
        assert_eq!(boots.len(), 2);
        assert!(boots.iter().all(|b| !b.sets.is_empty()));
        assert!(
            boots.iter().all(|b| !b.characters.is_empty()),
            "ready events carry the reporting roster"
        );
    }

    #[tokio::test]
    async fn definitive_failure_downgrades_to_conflict_only_and_disarms() {
        let source = two_event_source();
        let config = config_for(&[ULTIMATE, "tournament/fbr/event/nonexistent"]);

        let report = preflight(&source, &config, TIMEOUT, true, classify).await;

        assert!(report.fatal.is_none(), "one healthy event still launches");
        assert!(!report.writes_armed, "a failed event disarms writes");
        let failed = &report.brackets[1];
        assert!(matches!(failed.outcome, BracketOutcome::Failed { .. }));
        assert_eq!(failed.effective_mode(), BracketMode::ConflictOnly);
        assert!(report.render().contains("conflict-only"));

        let boots = report.into_bootstraps();
        assert_eq!(boots[1].mode, BracketMode::ConflictOnly);
        assert!(boots[1].sets.is_empty());
    }

    #[tokio::test]
    async fn connectivity_failure_launches_empty_in_configured_mode() {
        let mut source = two_event_source();
        source.set_hang(MELEE);
        let config = config_for(&[ULTIMATE, MELEE]);

        let report = preflight(&source, &config, TIMEOUT, true, classify).await;

        assert!(report.fatal.is_none());
        let offline = &report.brackets[1];
        assert!(matches!(offline.outcome, BracketOutcome::Offline { .. }), "{offline:?}");
        assert_eq!(
            offline.effective_mode(),
            BracketMode::Full,
            "connectivity keeps the configured mode"
        );
        assert!(!report.writes_armed, "not everything verified — stay disarmed");
    }

    #[tokio::test]
    async fn non_admin_token_disarms_writes() {
        let mut source = two_event_source();
        source.set_admin_probe(bracket_tools_startgg::AdminProbeResult {
            current_user: Some(42),
            admins: Some(vec![1, 2]),
        });
        let config = config_for(&[ULTIMATE, MELEE]);

        let report = preflight(&source, &config, TIMEOUT, true, classify).await;

        assert!(report.fatal.is_none());
        assert!(!report.writes_armed, "non-admin token must not arm writes");
        assert!(report.admin_probe.as_deref().is_some_and(|p| p.contains("not among")), "{report:?}");
        assert!(report.render().contains("advisor-only"));
    }

    #[tokio::test]
    async fn hidden_admin_list_disarms_writes() {
        let mut source = two_event_source();
        source.set_admin_probe(bracket_tools_startgg::AdminProbeResult {
            current_user: Some(42),
            admins: None,
        });
        let config = config_for(&[ULTIMATE, MELEE]);

        let report = preflight(&source, &config, TIMEOUT, true, classify).await;
        assert!(!report.writes_armed);
        assert!(report.admin_probe.as_deref().is_some_and(|p| p.contains("hidden")));
    }

    #[tokio::test]
    async fn advisor_only_without_pinned_called_int_escalates_soft_busy() {
        let source = two_event_source();
        let config = config_for(&[ULTIMATE, MELEE]);
        assert_eq!(config.known_called_state_int, None);

        // Not asking for writes at all: escalation still arms (detection is
        // just as blind), and the report says so.
        let report = preflight(&source, &config, TIMEOUT, false, classify).await;
        assert!(!report.writes_armed);
        assert!(report.escalate_soft_busy);
        assert!(report.render().contains("remote-call detection degraded"));

        // A pinned int quiets it.
        let mut pinned = config_for(&[ULTIMATE, MELEE]);
        pinned.known_called_state_int = Some(6);
        let report = preflight(&source, &pinned, TIMEOUT, false, classify).await;
        assert!(!report.escalate_soft_busy);
    }

    #[tokio::test]
    async fn all_failed_is_fatal() {
        let source = two_event_source();
        let config = config_for(&["tournament/fbr/event/nope1", "tournament/fbr/event/nope2"]);

        let report = preflight(&source, &config, TIMEOUT, true, classify).await;
        assert!(report.fatal.is_some());
        assert!(!report.writes_armed);
    }

    #[tokio::test]
    async fn tournament_identity_mismatch_is_fatal() {
        let ultimate = make_de_bracket(1001, 8);
        let other = make_de_bracket(2001, 4);
        let mut source = FixtureSource::new();
        source.add_synth_event(ULTIMATE, from_ref(&ultimate.info), vec![ultimate.sets]);
        source.add_synth_event("tournament/other/event/melee", from_ref(&other.info), vec![other.sets]);
        let config = config_for(&[ULTIMATE, "tournament/other/event/melee"]);

        let report = preflight(&source, &config, TIMEOUT, true, classify).await;
        assert!(
            report.fatal.as_deref().is_some_and(|f| f.contains("identity mismatch")),
            "{report:?}"
        );
    }

    #[tokio::test]
    async fn configured_tournament_slug_is_asserted() {
        let source = two_event_source();
        let mut config = config_for(&[ULTIMATE, MELEE]);
        config.tournament_slug = Some("tournament/some-other-major".to_owned());

        let report = preflight(&source, &config, TIMEOUT, true, classify).await;
        assert!(report.fatal.as_deref().is_some_and(|f| f.contains("tournament_slug")));
    }

    #[tokio::test]
    async fn expected_kind_mismatch_warns_but_launches() {
        let source = two_event_source();
        let mut config = config_for(&[ULTIMATE]);
        config.brackets[0].expected_kind = Some(ExpectedKind::Swiss);

        let report = preflight(&source, &config, TIMEOUT, true, classify).await;
        assert!(report.fatal.is_none());
        assert!(matches!(report.brackets[0].outcome, BracketOutcome::Ready { .. }));
        assert!(report.brackets[0].warnings.iter().any(|w| w.contains("expected_kind")));
    }

    #[tokio::test]
    async fn identity_split_scan_flags_unlinked_case_variant_tags() {
        let named = |prefix: &str, names: [&str; 4]| -> Vec<SynthPlayer> {
            names
                .iter()
                .enumerate()
                .map(|(i, name)| SynthPlayer {
                    player_id: format!("{prefix}{}", i + 1),
                    name: (*name).to_owned(),
                })
                .collect()
        };
        let ultimate_players = named("P", ["Wobbles", "Ally", "Dabuz", "Marss"]);
        let melee_players = named("Q", ["wobbles", "Mango", "Zain", "Plup"]);
        let ultimate = make_de_bracket_with(1001, &ultimate_players);
        let melee = make_de_bracket_with(2001, &melee_players);
        let mut source = FixtureSource::new();
        source.add_synth_event(ULTIMATE, from_ref(&ultimate.info), vec![ultimate.sets]);
        source.add_synth_event(MELEE, from_ref(&melee.info), vec![melee.sets]);
        let config = config_for(&[ULTIMATE, MELEE]);

        let report = preflight(&source, &config, TIMEOUT, true, classify).await;

        assert_eq!(report.identity_splits.len(), 1, "{:?}", report.identity_splits);
        assert_eq!(report.identity_splits[0].tag, "wobbles");
        assert!(report.render().contains("player_aliases"));
    }
}

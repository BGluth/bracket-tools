//! Fixture-replay suite against the S1 smoke captures: raw `GraphQlResponse`
//! envelopes → model conversion → graph build, per live event shape.
//!
//! The captures contain real player data and NEVER enter the repo; these
//! tests read them from `BRACKET_TOOLS_CAPTURES` (default:
//! `~/work/personal/bracket-tools-captures/2026-07-05_s1_smoke`) and skip
//! with a message when the directory is absent.

use std::{
    env, fs,
    path::{Path, PathBuf},
};

use bracket_tools_scheduler::{
    config::{BracketMode, SetupId},
    conflict::{callable_sets, AliasMap, BracketView, ConflictIndex, ConflictInputs, PlayerFlags, SetupBoard, Tombstones},
    graph::BracketGraph,
    model::{live_sets_from_schema, phase_groups_from_schema, GroupKind, LiveSet, PhaseGroupInfo},
};
use bracket_tools_startgg_schema::{get_event_structure::GetEventStructure, get_sets_for_event::GetSetsForEvent};
use cynic::GraphQlResponse;

const FBR_ULTIMATE: &str = "tournament_french-bread-rumble-100_event_ultimate-singles";
const RUST_VITATIONAL: &str = "tournament_rust_vitational_mk_xiii_event_ultimate-singles";
const FBR_POKEMON: &str = "tournament_french-bread-rumble-100_event_pokemon-champions-4v4-double-battle";

fn captures_dir() -> Option<PathBuf> {
    let dir = match env::var("BRACKET_TOOLS_CAPTURES") {
        Ok(dir) => PathBuf::from(dir),
        Err(_) => PathBuf::from(env::var("HOME").ok()?).join("work/personal/bracket-tools-captures/2026-07-05_s1_smoke"),
    };
    if dir.is_dir() {
        Some(dir)
    } else {
        eprintln!("skipping fixture replay: captures not found at {}", dir.display());
        None
    }
}

/// Replays one event's captured pages + structure through the conversion
/// layer, asserting zero skipped sets along the way.
fn load_event(dir: &Path, event: &str) -> (Vec<LiveSet>, Vec<PhaseGroupInfo>) {
    let event_dir = dir.join(event);

    let mut schema_sets = Vec::new();
    for page in 1.. {
        let path = event_dir.join(format!("sets_page_{page}.json"));
        let Ok(raw) = fs::read_to_string(&path) else {
            assert!(page > 1, "no capture pages under {}", event_dir.display());
            break;
        };
        let response: GraphQlResponse<GetSetsForEvent> = serde_json::from_str(&raw).unwrap_or_else(|e| panic!("{event} page {page}: {e}"));
        let nodes = response
            .data
            .and_then(|d| d.event)
            .and_then(|e| e.sets)
            .and_then(|s| s.nodes)
            .unwrap_or_else(|| panic!("{event} page {page}: empty envelope"));
        schema_sets.extend(nodes.into_iter().flatten());
    }

    let raw = fs::read_to_string(event_dir.join("structure.json")).expect("structure capture present");
    let response: GraphQlResponse<GetEventStructure> = serde_json::from_str(&raw).unwrap_or_else(|e| panic!("{event} structure: {e}"));
    let event_data = response.data.and_then(|d| d.event).expect("structure envelope has an event");
    let (groups, group_warnings) = phase_groups_from_schema(&event_data);
    assert!(group_warnings.is_empty(), "{event}: unexpected group warnings {group_warnings:?}");

    let (sets, _warnings, skipped) = live_sets_from_schema(schema_sets);
    assert!(skipped.is_empty(), "{event}: conversion skipped sets {skipped:?}");
    (sets, groups)
}

fn zero_callables(sets: &[LiveSet], groups: &[PhaseGroupInfo]) -> bool {
    let bracket = bracket_tools_scheduler::model::BracketId("replay".to_owned());
    let pool = [SetupId(1)];
    let board = SetupBoard::new(&pool);
    let (aliases, flags, tombstones) = (AliasMap::default(), PlayerFlags::default(), Tombstones::default());
    let (last_completed, snoozes) = (Default::default(), Default::default());
    let inputs = ConflictInputs {
        aliases: &aliases,
        board: &board,
        flags: &flags,
        tombstones: &tombstones,
        called_ints: &[6],
        soft_busy: &[],
        last_completed: &last_completed,
        rest_window_secs: 0,
        snoozes: &snoozes,
    };
    let views = [BracketView {
        id: &bracket,
        sets,
        mode: BracketMode::Full,
        start_at: None,
        held: false,
        pool: &pool,
    }];
    let index = ConflictIndex::build(&views, &inputs);
    let _ = groups;
    callable_sets(&views, &index, &inputs, 1_751_000_000_000).is_empty()
}

#[test]
fn every_captured_event_converts_with_zero_skips() {
    let Some(dir) = captures_dir() else {
        return;
    };
    let mut replayed = 0;
    for entry in fs::read_dir(&dir).expect("captures dir readable") {
        let entry = entry.expect("readable entry");
        if !entry.path().is_dir() || !entry.file_name().to_string_lossy().starts_with("tournament_") {
            continue;
        }
        // load_event asserts zero skips + clean structure internally.
        let (sets, groups) = load_event(&dir, &entry.file_name().to_string_lossy());
        assert!(!sets.is_empty());
        assert!(!groups.is_empty());
        replayed += 1;
    }
    assert!(replayed >= 10, "expected the full smoke sweep, found {replayed} events");
}

#[test]
fn fbr_ultimate_preview_skeleton_has_full_depth() {
    let Some(dir) = captures_dir() else {
        return;
    };
    let (sets, groups) = load_event(&dir, FBR_ULTIMATE);

    // The S1 smoke findings: 135 sets, every id still preview-form, the
    // empty future sets carrying the DAG spine.
    assert_eq!(sets.len(), 135);
    assert!(sets.iter().all(|s| s.id.0.starts_with("preview_")));

    let (graph, _warnings) = BracketGraph::build(&sets, &groups);
    let max_r1_depth = sets
        .iter()
        .enumerate()
        .filter(|(_, s)| s.key.round == 1)
        .map(|(i, _)| graph.depth(i))
        .max()
        .expect("round 1 exists");
    // The hideEmpty regression tripwire on live data: a 68-entrant DE's R1
    // loser route runs well past 10 incomplete sets.
    assert!(max_r1_depth >= 10, "got {max_r1_depth}");
    assert!(graph.remaining_critical_path() >= max_r1_depth);
}

#[test]
fn rust_vitational_mixed_types_build_and_completed_event_has_no_callables() {
    let Some(dir) = captures_dir() else {
        return;
    };
    let (sets, groups) = load_event(&dir, RUST_VITATIONAL);

    let kinds: Vec<&GroupKind> = groups.iter().map(|g| &g.kind).collect();
    assert!(
        kinds.iter().any(|k| **k == GroupKind::RoundRobin) && kinds.iter().any(|k| **k == GroupKind::Elimination),
        "expected mixed RR + DE typing, got {kinds:?}"
    );

    let (graph, _) = BracketGraph::build(&sets, &groups);
    assert_eq!(graph.remaining_critical_path(), 0, "completed event has nothing left");
    assert!(zero_callables(&sets, &groups), "completed event must yield zero callables");
}

#[test]
fn fbr_pokemon_swiss_plus_cut_builds_with_remaining_rounds_depth() {
    let Some(dir) = captures_dir() else {
        return;
    };
    let (sets, groups) = load_event(&dir, FBR_POKEMON);

    let swiss = groups
        .iter()
        .find(|g| matches!(g.kind, GroupKind::Swiss { .. }))
        .expect("a swiss group");
    let GroupKind::Swiss { num_rounds } = swiss.kind else {
        unreachable!();
    };
    assert!(groups.iter().any(|g| g.kind == GroupKind::Elimination), "and a top cut");

    let (graph, _) = BracketGraph::build(&sets, &groups);
    let swiss_stats = graph.group(&swiss.id).expect("swiss group indexed");
    // Unstarted swiss: current round open + every future round remaining.
    assert_eq!(swiss_stats.remaining_depth, num_rounds as u32);
    assert!(
        graph.remaining_critical_path() > swiss_stats.remaining_depth,
        "cut stage adds sequential depth"
    );
}

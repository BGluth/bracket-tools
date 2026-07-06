//! The recompute core: one pure pass from (remote snapshots + local overlay)
//! to everything the TUI displays — ranked queue, per-set block reasons,
//! per-bracket summaries, and projected finishes.
//!
//! `recompute` is synchronous and allocation-bounded; the Elm loop calls it
//! whenever state is dirty and renders straight off the returned [`World`].

use std::collections::{BTreeMap, HashMap};

use crate::{
    config::{BracketMode, SetupId, SimConfig},
    conflict::{
        aggregate_remaining, callable, callable_sets, AliasMap, BlockReason, BracketView, ConflictIndex, ConflictInputs, ConflictKey,
        PlayerFlags, SetupBoard, Tombstones, UnixMillis,
    },
    duration::DurationModel,
    graph::{BracketGraph, GraphWarning},
    model::{BracketId, LiveSet, PhaseGroupInfo, SetId, SetKey},
    ranker::{RankContext, RankedAction, RankedCandidate, Ranker},
    simulator::{simulate, SimBracket, SimWorld},
};

/// One bracket's inputs to a recompute: the latest remote snapshot plus the
/// per-bracket config the pipeline reads. The app owns these per event and
/// swaps `sets` wholesale on each successful poll.
#[derive(Debug, Clone)]
pub struct BracketState {
    pub id: BracketId,
    pub sets: Vec<LiveSet>,
    pub groups: Vec<PhaseGroupInfo>,
    pub mode: BracketMode,
    /// Effective open time in unix seconds (config override, else the
    /// structure query's start time).
    pub start_at: Option<i64>,
    pub held: bool,
    pub pool: Vec<SetupId>,
}

/// Everything a recompute reads. All references point into app-owned state;
/// nothing here is mutated.
pub struct WorldInputs<'a> {
    pub brackets: &'a [BracketState],
    pub board: &'a SetupBoard,
    pub flags: &'a PlayerFlags,
    pub tombstones: &'a Tombstones,
    pub aliases: &'a AliasMap,
    pub called_ints: &'a [i32],
    pub soft_busy: &'a [(BracketId, SetKey)],
    pub last_completed: &'a HashMap<ConflictKey, UnixMillis>,
    pub snoozes: &'a HashMap<(BracketId, SetKey), UnixMillis>,
    /// When each set first became ready (slots filled), keyed by the
    /// swap-stable [`SetKey`]; feeds the wait-time tiebreak.
    pub callable_since: &'a HashMap<SetKey, UnixMillis>,
    pub rest_window_secs: u64,
    pub sim: SimConfig,
    pub now_millis: UnixMillis,
}

/// A display-ready queue entry: the ranked candidate plus the labels the UI
/// needs without chasing set references.
#[derive(Debug, Clone, PartialEq)]
pub struct QueueEntry {
    pub candidate: RankedCandidate,
    pub bracket: BracketId,
    pub key: SetKey,
    pub id: SetId,
    pub candidate_setups: Vec<SetupId>,
    /// "Left vs Right" from occupant display names.
    pub players: String,
    pub round_text: String,
}

/// Per-bracket rollup for the summary pane.
#[derive(Debug, Clone, PartialEq)]
pub struct BracketSummary {
    pub id: BracketId,
    pub mode: BracketMode,
    /// Model-free remaining work: incomplete sets in the current snapshot.
    pub incomplete_sets: usize,
    /// Longest remaining sequential chain (graph stages summed).
    pub critical_path: u32,
    pub callable_now: usize,
    /// Projected finish (unix millis); absent for brackets the simulator
    /// doesn't schedule (conflict-only) or starved (blocked).
    pub projected_finish: Option<UnixMillis>,
    pub projection_blocked: bool,
    /// The projection folds in a bracket that hasn't started yet, so treat
    /// it as a floor, not a forecast.
    pub projection_includes_unstarted: bool,
}

/// The recompute output the TUI renders from.
#[derive(Debug, Clone, Default)]
pub struct World {
    /// The headline queue: best-first across every free setup, deduplicated
    /// by set (scores are setup-independent).
    pub queue: Vec<QueueEntry>,
    /// Ranking restricted to each free setup's permitted candidates — the
    /// call-picker modal's list.
    pub per_setup: BTreeMap<SetupId, Vec<QueueEntry>>,
    /// Why each incomplete, non-callable set can't be called right now
    /// (inspection view). ALL reasons are retained, not just the first.
    pub blocked: HashMap<(BracketId, SetKey), Vec<BlockReason>>,
    /// Alias-merged cross-bracket remaining counts (ironman index).
    pub remaining: HashMap<ConflictKey, u32>,
    /// Per bracket, in input order.
    pub summaries: Vec<BracketSummary>,
    /// Latest projected finish across fully-scheduled brackets.
    pub overall_projected_finish: Option<UnixMillis>,
    pub graph_warnings: Vec<(BracketId, GraphWarning)>,
}

impl World {
    /// The sets currently ready to call somewhere, as (bracket, key) pairs —
    /// the app stamps `callable_since` from this.
    pub fn callable_keys(&self) -> impl Iterator<Item = (&BracketId, &SetKey)> {
        self.queue.iter().map(|entry| (&entry.bracket, &entry.key))
    }
}

pub fn recompute(inputs: &WorldInputs<'_>, durations: &DurationModel, ranker: &impl Ranker) -> World {
    let mut graphs = HashMap::new();
    let mut graph_warnings = Vec::new();
    for bracket in inputs.brackets {
        let (graph, warnings) = BracketGraph::build(&bracket.sets, &bracket.groups);
        graph_warnings.extend(warnings.into_iter().map(|w| (bracket.id.clone(), w)));
        graphs.insert(bracket.id.clone(), graph);
    }

    let conflict_inputs = ConflictInputs {
        aliases: inputs.aliases,
        board: inputs.board,
        flags: inputs.flags,
        tombstones: inputs.tombstones,
        called_ints: inputs.called_ints,
        soft_busy: inputs.soft_busy,
        last_completed: inputs.last_completed,
        rest_window_secs: inputs.rest_window_secs,
        snoozes: inputs.snoozes,
    };
    let views: Vec<BracketView<'_>> = inputs
        .brackets
        .iter()
        .map(|bracket| BracketView {
            id: &bracket.id,
            sets: &bracket.sets,
            mode: bracket.mode,
            start_at: bracket.start_at,
            held: bracket.held,
            pool: &bracket.pool,
        })
        .collect();
    let index = ConflictIndex::build(&views, &conflict_inputs);
    let candidates = callable_sets(&views, &index, &conflict_inputs, inputs.now_millis);

    let mut blocked = HashMap::new();
    for (view, bracket) in views.iter().zip(inputs.brackets) {
        for set in bracket.sets.iter().filter(|s| !s.is_completed()) {
            if let Err(reasons) = callable(view, set, &index, &conflict_inputs, inputs.now_millis) {
                blocked.insert((bracket.id.clone(), set.key.clone()), reasons);
            }
        }
    }

    let graph_refs: Vec<_> = graphs.iter().collect();
    let remaining = aggregate_remaining(&graph_refs, inputs.aliases);
    let ctx = RankContext {
        graphs: &graphs,
        remaining: &remaining,
        aliases: inputs.aliases,
        callable_since: inputs.callable_since,
        now_millis: inputs.now_millis,
    };

    let mut per_setup = BTreeMap::new();
    for setup in inputs.board.free_ids() {
        let entries: Vec<QueueEntry> = ranker
            .rank(setup, &candidates, &ctx)
            .into_iter()
            .filter_map(|candidate| queue_entry(candidate, &graphs))
            .collect();
        per_setup.insert(setup, entries);
    }
    let queue = merged_queue(&per_setup);

    let sim_world = SimWorld {
        brackets: inputs
            .brackets
            .iter()
            .map(|bracket| SimBracket {
                id: bracket.id.clone(),
                sets: bracket.sets.clone(),
                groups: bracket.groups.clone(),
                mode: bracket.mode,
                start_at: bracket.start_at,
                held: bracket.held,
                pool: bracket.pool.clone(),
            })
            .collect(),
        board: inputs.board.clone(),
        flags: inputs.flags.clone(),
        tombstones: inputs.tombstones.clone(),
        called_ints: inputs.called_ints.to_vec(),
        aliases: inputs.aliases.clone(),
        soft_busy: inputs.soft_busy.to_vec(),
        last_completed: inputs.last_completed.clone(),
        rest_window_secs: inputs.rest_window_secs,
        sim: inputs.sim.clone(),
        now_millis: inputs.now_millis,
    };
    let outcome = simulate(&sim_world, durations);

    let summaries = inputs
        .brackets
        .iter()
        .map(|bracket| {
            let graph = &graphs[&bracket.id];
            BracketSummary {
                id: bracket.id.clone(),
                mode: bracket.mode,
                incomplete_sets: bracket.sets.iter().filter(|s| !s.is_completed()).count(),
                critical_path: graph.remaining_critical_path(),
                callable_now: candidates.iter().filter(|c| c.bracket == bracket.id).count(),
                projected_finish: outcome.per_bracket_finish.get(&bracket.id).copied(),
                projection_blocked: outcome.blocked.contains(&bracket.id),
                projection_includes_unstarted: outcome.includes_unstarted.contains(&bracket.id),
            }
        })
        .collect();

    World {
        queue,
        per_setup,
        blocked,
        remaining,
        summaries,
        overall_projected_finish: (!inputs.brackets.is_empty()).then_some(outcome.overall_finish),
        graph_warnings,
    }
}

/// Resolves a ranked candidate into a display entry. Hold actions carry no
/// set to resolve (rollout-only; they surface in S4's decision modal).
fn queue_entry(candidate: RankedCandidate, graphs: &HashMap<BracketId, BracketGraph>) -> Option<QueueEntry> {
    let RankedAction::Call(callable) = &candidate.action else {
        return None;
    };
    let (bracket, key, id, candidate_setups) = (
        callable.bracket.clone(),
        callable.key.clone(),
        callable.id.clone(),
        callable.candidate_setups.clone(),
    );

    let set = graphs.get(&bracket).and_then(|g| g.index_of_key(&key).map(|idx| &g.sets()[idx]));
    let players = set.map_or_else(String::new, |set| {
        set.occupants().map(|o| o.display_name.as_str()).collect::<Vec<_>>().join(" vs ")
    });
    let round_text = set
        .and_then(|s| s.full_round_text.clone())
        .unwrap_or_else(|| format!("Round {}", key.round));

    Some(QueueEntry {
        candidate,
        bracket,
        key,
        id,
        candidate_setups,
        players,
        round_text,
    })
}

/// Union of the per-setup rankings, deduplicated by set. Scores don't depend
/// on the setup, so any copy is representative; ordering re-sorts by score
/// with the ranker's deterministic tiebreak.
fn merged_queue(per_setup: &BTreeMap<SetupId, Vec<QueueEntry>>) -> Vec<QueueEntry> {
    let mut seen = HashMap::new();
    for entry in per_setup.values().flatten() {
        seen.entry((entry.bracket.clone(), entry.key.clone()))
            .or_insert_with(|| entry.clone());
    }
    let mut queue: Vec<QueueEntry> = seen.into_values().collect();
    queue.sort_by(|a, b| {
        b.candidate
            .score
            .total_cmp(&a.candidate.score)
            .then_with(|| tiebreak(a).cmp(&tiebreak(b)))
    });
    queue
}

fn tiebreak(entry: &QueueEntry) -> (&str, &str, i32, &str) {
    (&entry.bracket.0, &entry.key.phase_group, entry.key.round, &entry.key.identifier)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{recompute, BracketState, World, WorldInputs};
    use crate::{
        config::{BracketMode, SetupId, SimConfig},
        conflict::{AliasMap, BlockReason, PlayerFlags, SetupBoard, SetupStatus, Tombstones},
        duration::DurationModel,
        model::{BracketId, LiveSet},
        ranker::GreedyRanker,
        synth::{complete, make_de_bracket, make_rr_pool, SynthBracket},
    };

    const NOW: i64 = 1_751_000_000_000;

    struct Fixture {
        brackets: Vec<BracketState>,
        board: SetupBoard,
    }

    impl Fixture {
        fn new(brackets: Vec<(&str, SynthBracket)>, setups: Vec<SetupId>) -> Self {
            let brackets = brackets
                .into_iter()
                .map(|(id, bracket)| BracketState {
                    id: BracketId(id.to_owned()),
                    sets: bracket.sets,
                    groups: vec![bracket.info],
                    mode: BracketMode::Full,
                    start_at: None,
                    held: false,
                    pool: setups.clone(),
                })
                .collect();
            Self {
                brackets,
                board: SetupBoard::new(&setups),
            }
        }

        fn recompute(&self) -> World {
            let aliases = AliasMap::default();
            let flags = PlayerFlags::default();
            let tombstones = Tombstones::default();
            let (last_completed, snoozes, callable_since) = (HashMap::new(), HashMap::new(), HashMap::new());
            let inputs = WorldInputs {
                brackets: &self.brackets,
                board: &self.board,
                flags: &flags,
                tombstones: &tombstones,
                aliases: &aliases,
                called_ints: &[6],
                soft_busy: &[],
                last_completed: &last_completed,
                snoozes: &snoozes,
                callable_since: &callable_since,
                rest_window_secs: 0,
                sim: SimConfig::default(),
                now_millis: NOW,
            };
            recompute(&inputs, &DurationModel::new(), &GreedyRanker)
        }
    }

    fn sets_mut(fixture: &mut Fixture, bracket: usize) -> &mut Vec<LiveSet> {
        &mut fixture.brackets[bracket].sets
    }

    #[test]
    fn fresh_de_r1_has_full_chain_depth_and_projection() {
        let fixture = Fixture::new(vec![("ultimate", make_de_bracket(1001, 57))], vec![SetupId(1), SetupId(2)]);
        let world = fixture.recompute();

        assert!(!world.queue.is_empty());
        // The hideEmpty tripwire: a fresh 57-entrant DE's top candidate rides
        // the full R1 loser-route chain.
        assert!(world.queue[0].candidate.components.depth >= 10, "{:?}", world.queue[0].candidate);
        let summary = &world.summaries[0];
        assert!(summary.critical_path >= world.queue[0].candidate.components.depth);
        assert!(summary.incomplete_sets > 50);
        assert!(!summary.projection_blocked);
        let projected = summary.projected_finish.expect("scheduled bracket projects a finish");
        assert!(projected > NOW);
        assert_eq!(world.overall_projected_finish, Some(projected));
        // Display fields resolved.
        assert!(world.queue[0].players.contains(" vs "));
        assert!(!world.queue[0].round_text.is_empty());
    }

    #[test]
    fn completed_bracket_has_no_callables_and_no_remaining_work() {
        let mut bracket = make_rr_pool(2001, 4);
        for (i, set) in bracket.sets.iter_mut().enumerate() {
            complete(set, 0, 1_750_999_000 + i as i64);
        }
        let fixture = Fixture::new(vec![("pool", bracket)], vec![SetupId(1)]);
        let world = fixture.recompute();

        assert!(world.queue.is_empty());
        let summary = &world.summaries[0];
        assert_eq!(summary.incomplete_sets, 0);
        assert_eq!(summary.critical_path, 0);
        assert_eq!(summary.callable_now, 0);
        assert!(world.blocked.is_empty(), "completed sets don't collect block reasons");
    }

    #[test]
    fn player_mid_set_is_filtered_everywhere() {
        // Both brackets share default players P1..P8.
        let mut fixture = Fixture::new(
            vec![("ultimate", make_de_bracket(1001, 8)), ("melee", make_de_bracket(1002, 8))],
            vec![SetupId(1), SetupId(2)],
        );
        // The first ultimate R1 set is remotely in progress.
        let busy_names: Vec<String> = {
            let sets = sets_mut(&mut fixture, 0);
            sets[0].started_at = Some(NOW / 1000 - 300);
            sets[0].state_int = Some(2);
            sets[0].occupants().map(|o| o.display_name.clone()).collect()
        };
        let world = fixture.recompute();

        assert_eq!(busy_names.len(), 2);
        for entry in &world.queue {
            for name in entry.players.split(" vs ") {
                assert!(!busy_names.iter().any(|busy| busy == name), "busy player ranked: {entry:?}");
            }
        }
        // Their melee set is blocked with a retained PlayerBusy reason.
        let melee_blocked = world
            .blocked
            .iter()
            .filter(|((bracket, _), _)| bracket.0 == "melee")
            .find(|(_, reasons)| reasons.iter().any(|r| matches!(r, BlockReason::PlayerBusy { .. })));
        assert!(melee_blocked.is_some(), "expected a PlayerBusy block in melee: {:?}", world.blocked);
    }

    #[test]
    fn full_board_yields_empty_queue_with_reasons() {
        let mut fixture = Fixture::new(vec![("ultimate", make_de_bracket(1001, 8))], vec![SetupId(1)]);
        let occupied = SetupStatus::OccupiedExternal { set: None };
        fixture.board.set_status(SetupId(1), occupied);
        let world = fixture.recompute();

        assert!(world.queue.is_empty());
        assert!(world.per_setup.is_empty(), "no free setups to rank for");
        assert!(
            world
                .blocked
                .values()
                .any(|reasons| reasons.iter().any(|r| matches!(r, BlockReason::NoPermittedFreeSetup))),
            "ready sets report the missing setup: {:?}",
            world.blocked
        );
    }

    #[test]
    fn merged_queue_dedups_across_setups() {
        let fixture = Fixture::new(vec![("ultimate", make_de_bracket(1001, 8))], vec![SetupId(1), SetupId(2)]);
        let world = fixture.recompute();

        assert_eq!(world.per_setup.len(), 2);
        let per_setup_len = world.per_setup[&SetupId(1)].len();
        assert_eq!(world.per_setup[&SetupId(2)].len(), per_setup_len);
        assert_eq!(world.queue.len(), per_setup_len, "same sets on both setups merge");
        // Deterministic headline order matches the per-setup order.
        assert_eq!(world.queue, world.per_setup[&SetupId(1)]);
    }

    #[test]
    fn conflict_only_bracket_is_never_ranked_but_gates() {
        let mut fixture = Fixture::new(
            vec![("ultimate", make_de_bracket(1001, 8)), ("side", make_de_bracket(1002, 8))],
            vec![SetupId(1), SetupId(2)],
        );
        fixture.brackets[1].mode = BracketMode::ConflictOnly;
        let world = fixture.recompute();

        assert!(world.queue.iter().all(|e| e.bracket.0 == "ultimate"));
        let side = &world.summaries[1];
        assert_eq!(side.callable_now, 0);
        assert!(side.projected_finish.is_none(), "conflict-only brackets aren't scheduled");
        // Shared players still gate: give side's A-set a remote start and the
        // ultimate queue must drop them.
        let busy_names: Vec<String> = {
            let sets = sets_mut(&mut fixture, 1);
            sets[0].started_at = Some(NOW / 1000 - 60);
            sets[0].state_int = Some(2);
            sets[0].occupants().map(|o| o.display_name.clone()).collect()
        };
        let world = fixture.recompute();
        assert!(world
            .queue
            .iter()
            .all(|e| e.players.split(" vs ").all(|name| !busy_names.iter().any(|busy| busy == name))));
    }

    #[test]
    fn callable_keys_feed_wait_stamping() {
        let fixture = Fixture::new(vec![("ultimate", make_de_bracket(1001, 4))], vec![SetupId(1)]);
        let world = fixture.recompute();
        let keys: Vec<_> = world.callable_keys().collect();
        assert_eq!(keys.len(), world.queue.len());
    }
}

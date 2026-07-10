//! The recompute core: one pure pass from (remote snapshots + local overlay)
//! to everything the TUI displays — ranked queue, per-set block reasons,
//! per-bracket summaries, and projected finishes.
//!
//! `recompute` is synchronous and allocation-bounded; the Elm loop calls it
//! whenever state is dirty and renders straight off the returned [`World`].

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::{
    config::{BracketMode, SetupId, SimConfig},
    conflict::{
        aggregate_remaining, callable, callable_sets, effective_pool, AliasMap, BlockReason, BracketView, ConflictIndex, ConflictInputs,
        ConflictKey, PlayerFlags, PoolOverride, SetupBoard, SetupStatus, Tombstones, UnixMillis,
    },
    duration::DurationModel,
    graph::{BracketGraph, GraphWarning},
    model::{abbreviate_round, BracketId, LiveSet, PhaseGroupInfo, SetId, SetKey},
    ranker::{GreedyRanker, RankContext, RankedAction, RankedCandidate, Ranker},
    rollout::RolloutRanker,
    simulator::{simulate, SimBracket, SimOutcome, SimWorld},
};

/// How many greedy-leading candidates the decision-point rollout simulates
/// per setup (each costs one full forward simulation).
pub const ROLLOUT_TOP_K: usize = 8;

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
    /// The setup types this bracket may be called on; resolved against the
    /// live board roster at recompute time.
    pub setup_types: Vec<String>,
}

/// Everything a recompute reads. All references point into app-owned state;
/// nothing here is mutated.
pub struct WorldInputs<'a> {
    pub brackets: &'a [&'a BracketState],
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
    /// Per-setup reassignments (the `a` action); folded into every bracket's
    /// effective pool here.
    pub pool_overrides: &'a HashMap<SetupId, PoolOverride>,
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
    /// Setups whose every (effective-pool, full-mode) bracket is complete —
    /// candidates for reassignment, freeing, or friendlies.
    pub pool_exhausted: Vec<SetupId>,
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
    let pools: Vec<Vec<SetupId>> = inputs
        .brackets
        .iter()
        .map(|bracket| effective_pool(&bracket.id, &bracket.setup_types, inputs.board.setups(), inputs.pool_overrides))
        .collect();
    let views: Vec<BracketView<'_>> = inputs
        .brackets
        .iter()
        .zip(&pools)
        .map(|(bracket, pool)| BracketView {
            id: &bracket.id,
            sets: &bracket.sets,
            mode: bracket.mode,
            start_at: bracket.start_at,
            held: bracket.held,
            pool,
        })
        .collect();
    let index = ConflictIndex::build(&views, &conflict_inputs);
    let mut candidates = callable_sets(&views, &index, &conflict_inputs, inputs.now_millis);
    // The conflict predicate deliberately never self-blocks (commit-time
    // re-verification depends on that), so a set already assigned to a setup
    // still evaluates callable. Exclude board-assigned sets here instead —
    // they're on a station, not in the queue.
    let assigned = assigned_sets(inputs.board);
    candidates.retain(|c| !assigned.contains(&(c.bracket.clone(), c.key.clone())));

    let mut blocked = HashMap::new();
    for (view, bracket) in views.iter().zip(inputs.brackets) {
        for set in bracket
            .sets
            .iter()
            .filter(|s| !s.is_completed() && !assigned.contains(&(bracket.id.clone(), s.key.clone())))
        {
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
            .zip(&pools)
            .map(|(bracket, pool)| SimBracket {
                id: bracket.id.clone(),
                sets: bracket.sets.clone(),
                groups: bracket.groups.clone(),
                mode: bracket.mode,
                start_at: bracket.start_at,
                held: bracket.held,
                pool: pool.clone(),
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
    // Oversized worlds skip the projections sim (it walks the whole
    // remaining tournament): summaries lose their finish estimates but the
    // recompute stays interactive. Same knob gates rollout dispatch.
    let outcome = if world_within_sim_ceiling(&sim_world) {
        simulate(&sim_world, durations)
    } else {
        SimOutcome::default()
    };

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

    let pool_exhausted = exhausted_setups(inputs, &pools);

    World {
        queue,
        per_setup,
        blocked,
        remaining,
        summaries,
        overall_projected_finish: (!inputs.brackets.is_empty() && outcome.overall_finish != 0).then_some(outcome.overall_finish),
        pool_exhausted,
        graph_warnings,
    }
}

/// Setups every one of whose serving full-mode brackets has affirmatively
/// finished (a bracket with no sets yet — unstarted or unfetched — is not
/// "finished", so a pre-start lull never reads as exhaustion). A station
/// serving no bracket at all is plain free, not exhausted.
fn exhausted_setups(inputs: &WorldInputs<'_>, pools: &[Vec<SetupId>]) -> Vec<SetupId> {
    inputs
        .board
        .setups()
        .iter()
        .map(|s| s.id)
        .filter(|setup| {
            let serving: Vec<_> = inputs
                .brackets
                .iter()
                .zip(pools)
                .filter(|(bracket, pool)| bracket.mode == BracketMode::Full && pool.contains(setup))
                .collect();
            !serving.is_empty()
                && serving
                    .iter()
                    .all(|(bracket, _)| !bracket.sets.is_empty() && bracket.sets.iter().all(LiveSet::is_completed))
        })
        .collect()
}

/// An owned copy of everything a background rollout evaluation reads —
/// cloned off the app state so the simulator task borrows nothing.
#[derive(Debug, Clone)]
pub struct SimSnapshot {
    pub brackets: Vec<BracketState>,
    pub board: SetupBoard,
    pub flags: PlayerFlags,
    pub tombstones: Tombstones,
    pub aliases: AliasMap,
    pub called_ints: Vec<i32>,
    pub soft_busy: Vec<(BracketId, SetKey)>,
    pub last_completed: HashMap<ConflictKey, UnixMillis>,
    pub snoozes: HashMap<(BracketId, SetKey), UnixMillis>,
    pub callable_since: HashMap<SetKey, UnixMillis>,
    pub pool_overrides: HashMap<SetupId, PoolOverride>,
    pub rest_window_secs: u64,
    pub sim: SimConfig,
    pub now_millis: UnixMillis,
    pub durations: DurationModel,
}

/// One row of a rollout-ranked picker list: a call, or the epsilon-gated
/// HOLD with what it waits for. (Boxed: QueueEntry dwarfs the Hold variant.)
#[derive(Debug, Clone, PartialEq)]
pub enum RolloutRow {
    Call(Box<QueueEntry>),
    Hold {
        waiting_for: Option<SetKey>,
        projected_finish: Option<UnixMillis>,
    },
}

/// A background rollout evaluation's output, stamped with the world time it
/// was computed against.
#[derive(Debug, Clone, PartialEq)]
pub struct RolloutRankings {
    pub per_setup: BTreeMap<SetupId, Vec<RolloutRow>>,
    pub computed_at: UnixMillis,
}

/// The decision-point rollout: per free setup, greedy-order the candidates,
/// truncate to `top_k` (each costs a forward simulation), and rank those by
/// projected makespan via [`RolloutRanker`]. Pure and owned — built to run on
/// a blocking task off the Elm thread.
pub fn rollout_rankings(s: &SimSnapshot, top_k: usize) -> RolloutRankings {
    let mut graphs = HashMap::new();
    for bracket in &s.brackets {
        let (graph, _warnings) = BracketGraph::build(&bracket.sets, &bracket.groups);
        graphs.insert(bracket.id.clone(), graph);
    }

    let conflict_inputs = ConflictInputs {
        aliases: &s.aliases,
        board: &s.board,
        flags: &s.flags,
        tombstones: &s.tombstones,
        called_ints: &s.called_ints,
        soft_busy: &s.soft_busy,
        last_completed: &s.last_completed,
        rest_window_secs: s.rest_window_secs,
        snoozes: &s.snoozes,
    };
    let pools: Vec<Vec<SetupId>> = s
        .brackets
        .iter()
        .map(|bracket| effective_pool(&bracket.id, &bracket.setup_types, s.board.setups(), &s.pool_overrides))
        .collect();
    let views: Vec<BracketView<'_>> = s
        .brackets
        .iter()
        .zip(&pools)
        .map(|(bracket, pool)| BracketView {
            id: &bracket.id,
            sets: &bracket.sets,
            mode: bracket.mode,
            start_at: bracket.start_at,
            held: bracket.held,
            pool,
        })
        .collect();
    let index = ConflictIndex::build(&views, &conflict_inputs);
    let mut candidates = callable_sets(&views, &index, &conflict_inputs, s.now_millis);
    let assigned = assigned_sets(&s.board);
    candidates.retain(|c| !assigned.contains(&(c.bracket.clone(), c.key.clone())));

    let graph_refs: Vec<_> = graphs.iter().collect();
    let remaining = aggregate_remaining(&graph_refs, &s.aliases);
    let ctx = RankContext {
        graphs: &graphs,
        remaining: &remaining,
        aliases: &s.aliases,
        callable_since: &s.callable_since,
        now_millis: s.now_millis,
    };

    let sim_world = SimWorld {
        brackets: s
            .brackets
            .iter()
            .zip(&pools)
            .map(|(bracket, pool)| SimBracket {
                id: bracket.id.clone(),
                sets: bracket.sets.clone(),
                groups: bracket.groups.clone(),
                mode: bracket.mode,
                start_at: bracket.start_at,
                held: bracket.held,
                pool: pool.clone(),
            })
            .collect(),
        board: s.board.clone(),
        flags: s.flags.clone(),
        tombstones: s.tombstones.clone(),
        called_ints: s.called_ints.clone(),
        aliases: s.aliases.clone(),
        soft_busy: s.soft_busy.clone(),
        last_completed: s.last_completed.clone(),
        rest_window_secs: s.rest_window_secs,
        sim: s.sim.clone(),
        now_millis: s.now_millis,
    };
    let rollout = RolloutRanker {
        world: &sim_world,
        durations: &s.durations,
    };

    let mut per_setup = BTreeMap::new();
    for setup in s.board.free_ids() {
        let top: Vec<_> = GreedyRanker
            .rank(setup, &candidates, &ctx)
            .into_iter()
            .take(top_k)
            .filter_map(|entry| match entry.action {
                RankedAction::Call(callable) => Some(callable),
                RankedAction::Hold { .. } => None,
            })
            .collect();
        let rows: Vec<RolloutRow> = rollout
            .rank(setup, &top, &ctx)
            .into_iter()
            .filter_map(|candidate| match &candidate.action {
                RankedAction::Call(_) => queue_entry(candidate, &graphs).map(|e| RolloutRow::Call(Box::new(e))),
                RankedAction::Hold { waiting_for } => Some(RolloutRow::Hold {
                    waiting_for: waiting_for.clone(),
                    projected_finish: candidate.components.projected_finish,
                }),
            })
            .collect();
        per_setup.insert(setup, rows);
    }

    RolloutRankings {
        per_setup,
        computed_at: s.now_millis,
    }
}

/// The sets currently assigned to a setup (Called or InProgress on the
/// board) — excluded from ranking, and re-verified against at commit time.
pub fn assigned_sets(board: &SetupBoard) -> HashSet<(BracketId, SetKey)> {
    board
        .setups()
        .iter()
        .filter_map(|setup| match &setup.status {
            SetupStatus::Called { bracket, set } | SetupStatus::InProgress { bracket, set } => Some((bracket.clone(), set.clone())),
            SetupStatus::OccupiedExternal { set } => set.clone(),
            SetupStatus::Free => None,
        })
        .collect()
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
        .and_then(|s| s.full_round_text.as_deref().map(abbreviate_round))
        .unwrap_or_else(|| format!("R{}", key.round));

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

/// Whether the forward sims are affordable: `sim.world_ceiling` compared
/// against stations × incomplete sets (0 = unlimited).
fn within_sim_ceiling(ceiling: u64, stations: usize, open_sets: u64) -> bool {
    ceiling == 0 || stations as u64 * open_sets <= ceiling
}

fn world_within_sim_ceiling(world: &SimWorld) -> bool {
    let open = world
        .brackets
        .iter()
        .map(|b| b.sets.iter().filter(|s| !s.is_completed()).count() as u64)
        .sum();
    within_sim_ceiling(world.sim.world_ceiling, world.board.setups().len(), open)
}

/// The rollout-dispatch gate ([`SimSnapshot`] form; same knob as the
/// recompute's projections sim).
pub fn snapshot_within_sim_ceiling(snapshot: &SimSnapshot) -> bool {
    let open = snapshot
        .brackets
        .iter()
        .map(|b| b.sets.iter().filter(|s| !s.is_completed()).count() as u64)
        .sum();
    within_sim_ceiling(snapshot.sim.world_ceiling, snapshot.board.setups().len(), open)
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
        config::{BracketMode, SetupId, SimConfig, DEFAULT_SETUP_TYPE},
        conflict::{AliasMap, BlockReason, PlayerFlags, PoolOverride, SetupBoard, SetupStatus, Tombstones},
        duration::DurationModel,
        model::{BracketId, LiveSet},
        ranker::GreedyRanker,
        synth::{complete, make_de_bracket, make_rr_pool, make_se_bracket, SynthBracket},
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
                    setup_types: vec![DEFAULT_SETUP_TYPE.to_owned()],
                })
                .collect();
            Self {
                brackets,
                board: SetupBoard::new(&setups),
            }
        }

        fn recompute(&self) -> World {
            self.recompute_with_overrides(&HashMap::new())
        }

        fn recompute_with_overrides(&self, pool_overrides: &HashMap<SetupId, PoolOverride>) -> World {
            let aliases = AliasMap::default();
            let flags = PlayerFlags::default();
            let tombstones = Tombstones::default();
            let (last_completed, snoozes, callable_since) = (HashMap::new(), HashMap::new(), HashMap::new());
            let bracket_refs: Vec<&BracketState> = self.brackets.iter().collect();
            let inputs = WorldInputs {
                brackets: &bracket_refs,
                board: &self.board,
                flags: &flags,
                tombstones: &tombstones,
                aliases: &aliases,
                called_ints: &[6],
                soft_busy: &[],
                last_completed: &last_completed,
                snoozes: &snoozes,
                callable_since: &callable_since,
                pool_overrides,
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
    fn rollout_rankings_project_and_gate_hold() {
        use super::{rollout_rankings, RolloutRow, SimSnapshot};
        use crate::duration::CompletedSet;

        let fixture = Fixture::new(vec![("ultimate", make_de_bracket(1001, 8))], vec![SetupId(1), SetupId(2)]);
        let snapshot = |durations: DurationModel| SimSnapshot {
            brackets: fixture.brackets.clone(),
            board: fixture.board.clone(),
            flags: PlayerFlags::default(),
            tombstones: Tombstones::default(),
            aliases: AliasMap::default(),
            called_ints: vec![6],
            soft_busy: Vec::new(),
            last_completed: HashMap::new(),
            snoozes: HashMap::new(),
            callable_since: HashMap::new(),
            pool_overrides: HashMap::new(),
            rest_window_secs: 0,
            sim: SimConfig::default(),
            now_millis: NOW,
            durations,
        };

        // Pure priors: rollout ranks calls with projections, but never HOLD.
        let rankings = rollout_rankings(&snapshot(DurationModel::new()), 3);
        let rows = &rankings.per_setup[&SetupId(1)];
        assert!(!rows.is_empty() && rows.len() <= 3, "top-k bounded: {}", rows.len());
        for row in rows {
            match row {
                RolloutRow::Call(entry) => {
                    assert!(entry.candidate.components.projected_finish.is_some(), "projection carried");
                }
                RolloutRow::Hold { .. } => panic!("HOLD must not appear on pure priors"),
            }
        }

        // With a real observed sample, HOLD appears (epsilon-gated to last
        // place unless it genuinely wins).
        let mut durations = DurationModel::new();
        durations.ingest(
            &BracketId("ultimate".to_owned()),
            &CompletedSet {
                key: fixture.brackets[0].sets[0].key.clone(),
                id: fixture.brackets[0].sets[0].id.clone(),
                started_at: Some(NOW / 1000),
                completed_at: NOW / 1000 + 600,
            },
            Some(3),
            None,
            0,
        );
        let rankings = rollout_rankings(&snapshot(durations), 3);
        let rows = &rankings.per_setup[&SetupId(1)];
        assert!(
            rows.iter().any(|r| matches!(r, RolloutRow::Hold { .. })),
            "HOLD offered once samples exist: {rows:?}"
        );
        assert_eq!(rankings.computed_at, NOW);
    }

    #[test]
    fn pool_override_reshapes_candidates_and_exhaustion() {
        // An RR pool for melee: every set occupied from round one, so the
        // whole bracket can complete without winner propagation.
        let mut fixture = Fixture::new(
            vec![("ultimate", make_se_bracket(1001, 4)), ("melee", make_rr_pool(2001, 4))],
            vec![SetupId(1), SetupId(2)],
        );
        for set in sets_mut(&mut fixture, 1).iter_mut() {
            complete(set, 0, NOW / 1000 + 60);
        }

        // No overrides: nothing exhausted (setups still serve ultimate), and
        // ultimate may call on both setups.
        let world = fixture.recompute();
        assert!(world.pool_exhausted.is_empty());
        assert!(world.per_setup.contains_key(&SetupId(2)));

        // Dedicate setup 2 to the finished melee: it leaves ultimate's
        // effective pool (no candidates for it) and reads exhausted.
        let overrides = HashMap::from([(SetupId(2), PoolOverride::Dedicated(BracketId("melee".to_owned())))]);
        let world = fixture.recompute_with_overrides(&overrides);
        assert_eq!(world.pool_exhausted, vec![SetupId(2)]);
        assert!(
            world.per_setup.get(&SetupId(2)).is_none_or(Vec::is_empty),
            "dedicated setup offers nothing from other brackets: {:?}",
            world.per_setup.get(&SetupId(2))
        );
        assert!(
            world.queue.iter().all(|e| e.candidate_setups == vec![SetupId(1)]),
            "ultimate candidates lost setup 2: {:?}",
            world.queue
        );

        // AllowAny restores it as a candidate for ultimate.
        let overrides = HashMap::from([(SetupId(2), PoolOverride::AllowAny)]);
        let world = fixture.recompute_with_overrides(&overrides);
        assert!(world.queue.iter().all(|e| e.candidate_setups.contains(&SetupId(2))));
        assert!(world.pool_exhausted.is_empty(), "ultimate still runs on setup 2");
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

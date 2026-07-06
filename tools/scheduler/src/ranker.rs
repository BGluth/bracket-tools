//! Call-order policy: the [`Ranker`] trait and the greedy base
//! implementation. The rollout evaluator (S2 tail / S4) plugs in behind the
//! same trait.

use std::collections::HashMap;

use crate::{
    config::SetupId,
    conflict::{occupant_keys, AliasMap, CallableSet, ConflictKey, UnixMillis},
    graph::BracketGraph,
    model::{BracketId, SetKey},
};

/// Critical-path depth dominates: one depth unit outweighs any realistic sum
/// of the other terms.
pub const W_DEPTH: f64 = 100.0;
/// Ironman term: busiest cross-bracket player in the set.
pub const W_IRONMAN: f64 = 5.0;
/// Small bonus for sets that unblock more downstream sets.
pub const W_UNBLOCK: f64 = 1.0;
/// Soft wait-time tiebreak, per second callable (~83 min to equal one
/// ironman unit).
pub const W_WAIT: f64 = 0.001;

/// What a ranked entry proposes doing with the free setup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RankedAction {
    Call(CallableSet),
    /// Rollout only: leave the setup open, typically for whatever
    /// `waiting_for` frees up.
    Hold {
        waiting_for: Option<SetKey>,
    },
}

/// Score breakdown carried alongside every candidate so the UI can explain
/// the ordering instead of asserting it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScoreComponents {
    pub depth: u32,
    pub ironman: u32,
    pub unblock: u32,
    pub wait_secs: i64,
    /// Rollout only: projected overall finish if this action is taken now.
    pub projected_finish: Option<UnixMillis>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RankedCandidate {
    pub action: RankedAction,
    pub score: f64,
    pub components: ScoreComponents,
}

/// Everything the greedy policy reads. Assembled fresh per snapshot;
/// `callable_since` is an input here (S4 persists it).
pub struct RankContext<'a> {
    pub graphs: &'a HashMap<BracketId, BracketGraph>,
    /// Alias-merged cross-bracket remaining counts
    /// ([`crate::conflict::aggregate_remaining`]).
    pub remaining: &'a HashMap<ConflictKey, u32>,
    pub aliases: &'a AliasMap,
    /// When each set first became callable (unix millis), keyed by the
    /// swap-stable [`SetKey`].
    pub callable_since: &'a HashMap<SetKey, UnixMillis>,
    pub now_millis: UnixMillis,
}

/// Ranks the candidate calls for one free setup, best first.
pub trait Ranker {
    fn rank(&self, setup: SetupId, candidates: &[CallableSet], ctx: &RankContext<'_>) -> Vec<RankedCandidate>;
}

/// The base policy: weighted sum of structural terms, then a fully
/// deterministic ordering so identical worlds always produce identical call
/// sheets.
pub struct GreedyRanker;

impl Ranker for GreedyRanker {
    fn rank(&self, setup: SetupId, candidates: &[CallableSet], ctx: &RankContext<'_>) -> Vec<RankedCandidate> {
        let mut ranked: Vec<RankedCandidate> = candidates
            .iter()
            .filter(|c| c.candidate_setups.contains(&setup))
            .map(|c| score_candidate(c, ctx))
            .collect();
        ranked.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| deterministic_key(&a.action).cmp(&deterministic_key(&b.action)))
        });
        ranked
    }
}

fn score_candidate(candidate: &CallableSet, ctx: &RankContext<'_>) -> RankedCandidate {
    let components = components_for(candidate, ctx);
    let score = W_DEPTH * components.depth as f64
        + W_IRONMAN * components.ironman as f64
        + W_UNBLOCK * components.unblock as f64
        + W_WAIT * components.wait_secs as f64;
    RankedCandidate {
        action: RankedAction::Call(candidate.clone()),
        score,
        components,
    }
}

fn components_for(candidate: &CallableSet, ctx: &RankContext<'_>) -> ScoreComponents {
    let wait_secs = ctx
        .callable_since
        .get(&candidate.key)
        .map_or(0, |&since| (ctx.now_millis - since).max(0) / 1000);

    // A candidate whose graph vanished mid-snapshot scores zero components
    // rather than disappearing from the sheet.
    let Some((graph, idx)) = ctx
        .graphs
        .get(&candidate.bracket)
        .and_then(|g| g.index_of_key(&candidate.key).map(|idx| (g, idx)))
    else {
        return ScoreComponents {
            wait_secs,
            ..ScoreComponents::default()
        };
    };

    let ironman = graph.sets()[idx]
        .occupants()
        .flat_map(|o| occupant_keys(o, ctx.aliases))
        .filter_map(|key| ctx.remaining.get(&key))
        .copied()
        .max()
        .unwrap_or(0);

    ScoreComponents {
        depth: graph.depth(idx),
        ironman,
        unblock: graph.unblock_count(idx),
        wait_secs,
        projected_finish: None,
    }
}

/// Final ordering for score ties: `(bracket, phase_group, round, identifier)`.
/// Holds sort last among equals.
fn deterministic_key(action: &RankedAction) -> (u8, String, String, i32, String) {
    match action {
        RankedAction::Call(c) => (
            0,
            c.bracket.0.clone(),
            c.key.phase_group.clone(),
            c.key.round,
            c.key.identifier.clone(),
        ),
        RankedAction::Hold { .. } => (1, String::new(), String::new(), 0, String::new()),
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, slice::from_ref};

    use super::{GreedyRanker, RankContext, RankedAction, RankedCandidate, Ranker};
    use crate::{
        config::{BracketMode, SetupId},
        conflict::{
            aggregate_remaining, callable_sets, AliasMap, BracketView, ConflictIndex, ConflictInputs, PlayerFlags, SetupBoard, Tombstones,
        },
        graph::BracketGraph,
        model::{BracketId, SetKey},
        synth::{make_de_bracket, make_de_bracket_with, SynthBracket, SynthPlayer},
    };

    const NOW: i64 = 1_751_000_000_000;

    fn players(prefix: &str, n: usize) -> Vec<SynthPlayer> {
        (1..=n)
            .map(|i| SynthPlayer {
                player_id: format!("{prefix}{i}"),
                name: format!("{prefix} {i}"),
            })
            .collect()
    }

    /// Full pipeline: graphs → conflict index → callables → rank.
    struct RankWorld {
        brackets: Vec<(BracketId, SynthBracket)>,
        pool: Vec<SetupId>,
        callable_since: HashMap<SetKey, i64>,
    }

    impl RankWorld {
        fn new(brackets: Vec<(&str, SynthBracket)>) -> Self {
            Self {
                brackets: brackets.into_iter().map(|(id, b)| (BracketId(id.to_owned()), b)).collect(),
                pool: vec![SetupId(1)],
                callable_since: HashMap::new(),
            }
        }

        fn rank(&self) -> Vec<RankedCandidate> {
            let aliases = AliasMap::default();
            let board = SetupBoard::new(&self.pool);
            let flags = PlayerFlags::default();
            let tombstones = Tombstones::default();
            let (last_completed, snoozes) = (HashMap::new(), HashMap::new());
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

            let views: Vec<BracketView<'_>> = self
                .brackets
                .iter()
                .map(|(id, bracket)| BracketView {
                    id,
                    sets: &bracket.sets,
                    mode: BracketMode::Full,
                    start_at: None,
                    held: false,
                    pool: &self.pool,
                })
                .collect();
            let index = ConflictIndex::build(&views, &inputs);
            let candidates = callable_sets(&views, &index, &inputs, NOW);

            let graphs: HashMap<BracketId, BracketGraph> = self
                .brackets
                .iter()
                .map(|(id, bracket)| {
                    let (graph, _) = BracketGraph::build(&bracket.sets, from_ref(&bracket.info));
                    (id.clone(), graph)
                })
                .collect();
            let graph_refs: Vec<_> = graphs.iter().map(|(id, g)| (id, g)).collect();
            let remaining = aggregate_remaining(&graph_refs, &aliases);

            let ctx = RankContext {
                graphs: &graphs,
                remaining: &remaining,
                aliases: &aliases,
                callable_since: &self.callable_since,
                now_millis: NOW,
            };
            GreedyRanker.rank(SetupId(1), &candidates, &ctx)
        }
    }

    fn called_keys(ranked: &[RankedCandidate]) -> Vec<(String, String)> {
        ranked
            .iter()
            .map(|r| match &r.action {
                RankedAction::Call(c) => (c.bracket.0.clone(), c.key.identifier.clone()),
                RankedAction::Hold { .. } => panic!("greedy never holds"),
            })
            .collect()
    }

    #[test]
    fn identical_worlds_rank_identically() {
        let make = || {
            RankWorld::new(vec![
                ("ultimate", make_de_bracket_with(20, &players("U", 8))),
                ("mugen", make_de_bracket_with(30, &players("M", 4))),
            ])
        };
        let (a, b) = (make().rank(), make().rank());
        assert!(!a.is_empty());
        assert_eq!(a, b);
    }

    #[test]
    fn deeper_bracket_outranks_shallower() {
        let world = RankWorld::new(vec![
            ("mugen", make_de_bracket_with(30, &players("M", 4))),
            ("ultimate", make_de_bracket_with(20, &players("U", 8))),
        ]);
        let ranked = world.rank();

        // Every callable R1 set in the 8-bracket (depth 6) precedes every one
        // in the 4-bracket (depth 4).
        let order = called_keys(&ranked);
        let last_ultimate = order.iter().rposition(|(b, _)| b == "ultimate").unwrap();
        let first_mugen = order.iter().position(|(b, _)| b == "mugen").unwrap();
        assert!(last_ultimate < first_mugen, "order: {order:?}");
        assert_eq!(ranked[0].components.depth, 6);
        assert_eq!(ranked[0].components.unblock, 2, "R1 feeds W2 and L1");
    }

    #[test]
    fn ironman_prefers_the_multi_bracket_player() {
        // Same-shape brackets; only M1 plays in both (as U1 there).
        let mut melee_players = players("M", 4);
        melee_players[0].player_id = "U1".to_owned();
        let world = RankWorld::new(vec![
            ("ultimate", make_de_bracket_with(20, &players("U", 4))),
            ("melee", make_de_bracket_with(30, &melee_players)),
        ]);
        let ranked = world.rank();

        let order = called_keys(&ranked);
        assert_eq!(
            &order[..2],
            &[("melee".to_owned(), "A".to_owned()), ("ultimate".to_owned(), "A".to_owned())],
            "U1's two sets (ironman 2) lead, tie broken by bracket name: {order:?}"
        );
        assert_eq!(ranked[0].components.ironman, 2);
        assert_eq!(ranked[2].components.ironman, 1);
    }

    #[test]
    fn ties_break_deterministically_and_wait_time_nudges() {
        let bracket = make_de_bracket(20, 4);
        let key_b = bracket.sets[1].key.clone();
        let mut world = RankWorld::new(vec![("ultimate", bracket)]);

        // Pure tie: (bracket, phase_group, round, identifier) ordering.
        let order = called_keys(&world.rank());
        assert_eq!(order[0].1, "A");
        assert_eq!(order[1].1, "B");

        // A long-waiting B overtakes A without touching the structural terms.
        world.callable_since.insert(key_b, NOW - 3_600_000);
        let ranked = world.rank();
        let order = called_keys(&ranked);
        assert_eq!(order[0].1, "B");
        assert_eq!(ranked[0].components.wait_secs, 3600);
        assert_eq!(ranked[0].components.depth, ranked[1].components.depth);
    }

    #[test]
    fn rank_filters_by_setup_permission() {
        let bracket = make_de_bracket(20, 4);
        let world = RankWorld::new(vec![("ultimate", bracket)]);
        // Rebuild context by hand to rank against a setup outside the pool.
        let ranked = world.rank();
        assert!(!ranked.is_empty());

        let graphs = HashMap::new();
        let remaining = HashMap::new();
        let aliases = AliasMap::default();
        let callable_since = HashMap::new();
        let ctx = RankContext {
            graphs: &graphs,
            remaining: &remaining,
            aliases: &aliases,
            callable_since: &callable_since,
            now_millis: NOW,
        };
        let candidates: Vec<_> = ranked
            .iter()
            .map(|r| match &r.action {
                RankedAction::Call(c) => c.clone(),
                RankedAction::Hold { .. } => unreachable!(),
            })
            .collect();
        assert!(
            GreedyRanker.rank(SetupId(99), &candidates, &ctx).is_empty(),
            "no candidate is permitted on setup 99"
        );
    }

    #[test]
    fn default_player_overlap_is_visible_to_ironman() {
        // Both brackets use default players P1..P4: everyone irons.
        let world = RankWorld::new(vec![("ultimate", make_de_bracket(20, 4)), ("melee", make_de_bracket(30, 4))]);
        let ranked = world.rank();
        assert_eq!(ranked.len(), 4);
        assert!(ranked.iter().all(|r| r.components.ironman == 2), "every player plays both brackets");
    }
}

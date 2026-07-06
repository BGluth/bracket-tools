//! Rollout evaluator: ranks a free setup's options — every callable set plus
//! HOLD — by the projected overall makespan of forward-simulating each one,
//! behind the same [`Ranker`] trait as the greedy policy. S4 adds the wiring
//! (decision-point triggers, top-K display, modal policy); the logic lives
//! here.

use crate::{
    config::SetupId,
    conflict::CallableSet,
    duration::DurationModel,
    model::SetKey,
    ranker::{GreedyRanker, RankContext, RankedAction, RankedCandidate, Ranker, ScoreComponents},
    simulator::{simulate_action, Action, SimWorld},
};

/// Ranks by projected makespan, with the greedy order as the within-noise
/// tie-break. HOLD is epsilon-gated: it never leads while inside the noise
/// band of the best call, and it is never offered at all while durations are
/// pure priors (no observed samples yet).
pub struct RolloutRanker<'a> {
    pub world: &'a SimWorld,
    pub durations: &'a DurationModel,
}

impl Ranker for RolloutRanker<'_> {
    fn rank(&self, setup: SetupId, candidates: &[CallableSet], ctx: &RankContext<'_>) -> Vec<RankedCandidate> {
        let greedy = GreedyRanker.rank(setup, candidates, ctx);
        if greedy.is_empty() {
            return greedy;
        }
        let now = self.world.now_millis;

        let evaluated: Vec<(RankedCandidate, i64, usize)> = greedy
            .into_iter()
            .enumerate()
            .map(|(greedy_pos, mut entry)| {
                let RankedAction::Call(callable) = &entry.action else {
                    unreachable!("greedy ranks calls only");
                };
                let outcome = simulate_action(
                    self.world,
                    self.durations,
                    &Action::Call {
                        bracket: callable.bracket.clone(),
                        set: callable.key.clone(),
                        setup,
                    },
                );
                // Structural components stay for the UI; the score becomes
                // the projection (negated seconds-from-now: higher = better).
                entry.components.projected_finish = Some(outcome.overall_finish);
                entry.score = -((outcome.overall_finish - now) as f64 / 1000.0);
                (entry, outcome.overall_finish, greedy_pos)
            })
            .collect();

        let best = evaluated.iter().map(|(_, makespan, _)| *makespan).min().expect("non-empty");
        let band = (((best - now).max(0)) as f64 * self.world.sim.noise_epsilon) as i64;

        // Within the noise band of the best projection, the deterministic
        // greedy order decides; beyond it, the projection does.
        let mut evaluated = evaluated;
        evaluated.sort_by_key(|(_, makespan, greedy_pos)| {
            if *makespan <= best + band {
                (0, 0, *greedy_pos)
            } else {
                (1, *makespan, *greedy_pos)
            }
        });
        let mut ranked: Vec<RankedCandidate> = evaluated.into_iter().map(|(entry, ..)| entry).collect();

        if self.durations.has_samples() {
            let outcome = simulate_action(self.world, self.durations, &Action::Hold { setup });
            let hold = RankedCandidate {
                action: RankedAction::Hold {
                    waiting_for: self.next_expected_completion(),
                },
                score: -((outcome.overall_finish - now) as f64 / 1000.0),
                components: ScoreComponents {
                    projected_finish: Some(outcome.overall_finish),
                    ..ScoreComponents::default()
                },
            };
            // HOLD leads only when it beats the best call beyond the noise.
            if outcome.overall_finish < best - band {
                ranked.insert(0, hold);
            } else {
                ranked.push(hold);
            }
        }
        ranked
    }
}

impl RolloutRanker<'_> {
    /// What a HOLD is waiting on: the earliest-started remotely-active set
    /// (the next expected completion, to a first approximation).
    fn next_expected_completion(&self) -> Option<SetKey> {
        self.world
            .brackets
            .iter()
            .flat_map(|b| b.sets.iter())
            .filter(|s| s.is_remotely_active())
            .min_by_key(|s| (s.started_at, s.key.clone()))
            .map(|s| s.key.clone())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::RolloutRanker;
    use crate::{
        config::{BracketMode, SetupId, SimConfig},
        conflict::{aggregate_remaining, AliasMap, CallableSet, ConflictKey, PlayerFlags, SetupBoard, Tombstones, UnixMillis},
        duration::{CompletedSet, DurationModel},
        graph::BracketGraph,
        model::{BracketId, SetKey},
        ranker::{RankContext, RankedAction, RankedCandidate, Ranker},
        simulator::{SimBracket, SimWorld},
        synth::{make_de_bracket, make_rr_pool},
    };

    const NOW: i64 = 1_751_000_000_000;

    fn world(brackets: Vec<SimBracket>, setups: &[SetupId]) -> SimWorld {
        SimWorld {
            brackets,
            board: SetupBoard::new(setups),
            flags: PlayerFlags::default(),
            tombstones: Tombstones::default(),
            called_ints: vec![6],
            aliases: AliasMap::default(),
            soft_busy: Vec::new(),
            last_completed: HashMap::new(),
            rest_window_secs: 0,
            sim: SimConfig::default(),
            now_millis: NOW,
        }
    }

    /// A duration model with one real sample that leaves the 480s estimate
    /// untouched — opens the HOLD gate without moving any projection.
    fn sampled_durations(bracket: &str) -> DurationModel {
        let mut durations = DurationModel::new();
        let sample = CompletedSet {
            key: SetKey {
                phase_group: "s".to_owned(),
                round: 1,
                identifier: "X".to_owned(),
            },
            id: crate::model::SetId("sample".to_owned()),
            started_at: Some(NOW / 1000 - 1000),
            completed_at: NOW / 1000 - 520,
        };
        durations.ingest(&BracketId(bracket.to_owned()), &sample, Some(3), None, 0);
        durations
    }

    struct Ctx {
        graphs: HashMap<BracketId, BracketGraph>,
        remaining: HashMap<ConflictKey, u32>,
        aliases: AliasMap,
        callable_since: HashMap<SetKey, UnixMillis>,
    }

    impl Ctx {
        fn new(world: &SimWorld) -> Self {
            let aliases = AliasMap::default();
            let graphs: HashMap<BracketId, BracketGraph> = world
                .brackets
                .iter()
                .map(|b| (b.id.clone(), BracketGraph::build(&b.sets, &b.groups).0))
                .collect();
            let graph_refs: Vec<_> = graphs.iter().collect();
            let remaining = aggregate_remaining(&graph_refs, &aliases);
            Self {
                graphs,
                remaining,
                aliases,
                callable_since: HashMap::new(),
            }
        }

        fn rank_context(&self) -> RankContext<'_> {
            RankContext {
                graphs: &self.graphs,
                remaining: &self.remaining,
                aliases: &self.aliases,
                callable_since: &self.callable_since,
                now_millis: NOW,
            }
        }
    }

    /// The engineered HOLD-wins world: main DE(4) is pinned to setup 1 (its
    /// only permitted station) with both W1 sets finishing remotely in 60s;
    /// the side pool set may run anywhere. Calling side on setup 1 now would
    /// stall main's whole chain behind it.
    fn hold_scenario() -> (SimWorld, Vec<CallableSet>) {
        let mut main = make_de_bracket(9, 4);
        main.sets[0].started_at = Some(NOW / 1000 - 420);
        main.sets[1].started_at = Some(NOW / 1000 - 400);
        let side = make_rr_pool(30, 2);
        let side_set = &side.sets[0];

        let candidates = vec![CallableSet {
            bracket: BracketId("side".to_owned()),
            key: side_set.key.clone(),
            id: side_set.id.clone(),
            candidate_setups: vec![SetupId(1), SetupId(2)],
        }];

        let brackets = vec![
            SimBracket {
                id: BracketId("main".to_owned()),
                sets: main.sets,
                groups: vec![main.info],
                mode: BracketMode::Full,
                start_at: None,
                held: false,
                pool: vec![SetupId(1)],
            },
            SimBracket {
                id: BracketId("side".to_owned()),
                sets: side.sets,
                groups: vec![side.info],
                mode: BracketMode::Full,
                start_at: None,
                held: false,
                pool: vec![SetupId(1), SetupId(2)],
            },
        ];
        (world(brackets, &[SetupId(1), SetupId(2)]), candidates)
    }

    fn actions(ranked: &[RankedCandidate]) -> Vec<&RankedAction> {
        ranked.iter().map(|r| &r.action).collect()
    }

    #[test]
    fn hold_wins_when_calling_would_stall_the_critical_bracket() {
        let (world, candidates) = hold_scenario();
        let durations = sampled_durations("side");
        let ctx = Ctx::new(&world);
        let ranker = RolloutRanker {
            world: &world,
            durations: &durations,
        };

        let ranked = ranker.rank(SetupId(1), &candidates, &ctx.rank_context());
        let RankedAction::Hold { waiting_for } = &ranked[0].action else {
            panic!("expected HOLD first, got {:?}", ranked[0]);
        };
        assert_eq!(
            waiting_for.as_ref(),
            Some(&world.brackets[0].sets[0].key),
            "waiting on the earliest-started active set"
        );

        // Deltas are exposed: holding beats calling by a real margin.
        let hold_finish = ranked[0].components.projected_finish.unwrap();
        let call_finish = ranked[1].components.projected_finish.unwrap();
        assert!(hold_finish + 300_000 <= call_finish, "hold {hold_finish} vs call {call_finish}");
    }

    #[test]
    fn hold_is_never_offered_on_pure_priors() {
        let (world, candidates) = hold_scenario();
        let durations = DurationModel::new();
        let ctx = Ctx::new(&world);
        let ranker = RolloutRanker {
            world: &world,
            durations: &durations,
        };

        let ranked = ranker.rank(SetupId(1), &candidates, &ctx.rank_context());
        assert!(
            ranked.iter().all(|r| matches!(r.action, RankedAction::Call(_))),
            "no HOLD while durations are pure priors: {ranked:?}"
        );
    }

    #[test]
    fn within_noise_ties_fall_back_to_greedy_order_and_hold_sits_last() {
        // Fresh symmetric DE(4), both W1 sets callable on both setups:
        // either first call projects the same makespan.
        let bracket = make_de_bracket(9, 4);
        let candidates: Vec<CallableSet> = bracket.sets[..2]
            .iter()
            .map(|s| CallableSet {
                bracket: BracketId("melee".to_owned()),
                key: s.key.clone(),
                id: s.id.clone(),
                candidate_setups: vec![SetupId(1), SetupId(2)],
            })
            .collect();
        let world = world(
            vec![SimBracket {
                id: BracketId("melee".to_owned()),
                sets: bracket.sets,
                groups: vec![bracket.info],
                mode: BracketMode::Full,
                start_at: None,
                held: false,
                pool: vec![SetupId(1), SetupId(2)],
            }],
            &[SetupId(1), SetupId(2)],
        );
        let durations = sampled_durations("melee");
        let ctx = Ctx::new(&world);
        let ranker = RolloutRanker {
            world: &world,
            durations: &durations,
        };

        let ranked = ranker.rank(SetupId(1), &candidates, &ctx.rank_context());
        let order = actions(&ranked);
        let RankedAction::Call(first) = order[0] else {
            panic!("call first");
        };
        let RankedAction::Call(second) = order[1] else {
            panic!("call second");
        };
        assert_eq!(first.key.identifier, "A", "greedy order breaks the projection tie");
        assert_eq!(second.key.identifier, "B");
        assert!(
            matches!(order.last(), Some(RankedAction::Hold { .. })),
            "hold gains nothing here, so it sits last: {order:?}"
        );
        assert_eq!(
            ranked[0].components.projected_finish, ranked[1].components.projected_finish,
            "symmetric world, symmetric projections"
        );
    }
}

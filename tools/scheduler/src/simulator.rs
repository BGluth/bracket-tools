//! Greedy forward-simulation engine: projects when everything finishes by
//! replaying the world forward with the *real* callable predicate and greedy
//! ranker under a relaxed regime (snoozes stripped, resting players return
//! after the sim horizon, remote evidence resolved to concrete finish times).
//!
//! Everything is deterministic: an event queue keyed by (time, sequence),
//! slot-0 winner propagation, and the ranker's deterministic ordering. The
//! hard no-progress guard marks starved brackets `blocked` instead of
//! looping.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::{
    config::{BracketMode, SetupId, SimConfig},
    conflict::{
        aggregate_remaining, callable_sets, occupant_keys, AliasMap, BracketView, ConflictIndex, ConflictInputs, ConflictKey, PlayerFlags,
        SetupBoard, SetupStatus, Tombstones, UnixMillis,
    },
    duration::DurationModel,
    graph::BracketGraph,
    model::{BracketId, EntrantId, GroupKind, LiveSet, PhaseGroupInfo, Prereq, SetId, SetKey, Slot, SlotOccupant},
    ranker::{GreedyRanker, RankContext, RankedAction, Ranker, ScoreComponents},
};

/// A remotely-active set never finishes sooner than this from "now" — we
/// don't know how far along it is.
pub const REMOTE_ACTIVE_MIN_REMAINDER_SECS: i64 = 60;

/// One bracket's full state for simulation. `start_at` already carries any
/// config override.
#[derive(Debug, Clone)]
pub struct SimBracket {
    pub id: BracketId,
    pub sets: Vec<LiveSet>,
    pub groups: Vec<PhaseGroupInfo>,
    pub mode: BracketMode,
    pub start_at: Option<i64>,
    pub held: bool,
    pub pool: Vec<SetupId>,
}

/// The complete simulation input; cloned internally, never mutated.
#[derive(Debug, Clone)]
pub struct SimWorld {
    pub brackets: Vec<SimBracket>,
    pub board: SetupBoard,
    pub flags: PlayerFlags,
    pub tombstones: Tombstones,
    pub called_ints: Vec<i32>,
    pub aliases: AliasMap,
    pub soft_busy: Vec<(BracketId, SetKey)>,
    pub last_completed: HashMap<ConflictKey, UnixMillis>,
    pub rest_window_secs: u64,
    pub sim: SimConfig,
    pub now_millis: UnixMillis,
}

/// The rollout seam: force one first decision, then simulate normally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Call {
        bracket: BracketId,
        set: SetKey,
        setup: SetupId,
    },
    /// Leave the setup idle until the next event.
    Hold {
        setup: SetupId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimOutcome {
    /// Latest projected finish across fully-scheduled brackets (unix millis;
    /// `now` when nothing was left to run).
    pub overall_finish: UnixMillis,
    /// Per fully-scheduled bracket; blocked brackets are absent.
    pub per_bracket_finish: HashMap<BracketId, UnixMillis>,
    /// Brackets the no-progress guard starved: nothing assignable, nothing
    /// in flight, incomplete sets remain.
    pub blocked: Vec<BracketId>,
    /// Label: these projections include demand from brackets that hadn't
    /// started yet.
    pub includes_unstarted: Vec<BracketId>,
}

/// One recorded step of a simulated run: the full set table of the bracket
/// that just changed, stamped with the sim clock. A frame sequence is a
/// scripted timeline of the tournament — the paced `--simulate` rehearsal
/// replays it through [`crate::fixture_source::FixtureSource`].
#[derive(Debug, Clone, PartialEq)]
pub struct ScriptFrame {
    pub at: UnixMillis,
    pub bracket: BracketId,
    pub sets: Vec<LiveSet>,
}

/// One auto-played step for the `--autoplay` replay: every greedy call the
/// sim commits (with its score ingredients — the "why") and every completion.
#[derive(Debug, Clone, PartialEq)]
pub enum ReplayEvent {
    Call {
        at: UnixMillis,
        setup: SetupId,
        bracket: BracketId,
        key: SetKey,
        players: String,
        round_text: String,
        components: ScoreComponents,
        /// The next-best candidate this call beat, for "why this over that".
        runner_up: Option<RunnerUp>,
        /// The called set's bracket neighborhood.
        context: SetContext,
        /// When the sim expects the set to finish.
        est_finish: UnixMillis,
    },
    Complete {
        at: UnixMillis,
        bracket: BracketId,
        key: SetKey,
        players: String,
        winner: String,
        /// Where the winner landed ("Grand Final (A) vs Carol"), when the
        /// bracket has a next set for them.
        winner_to: Option<String>,
        /// Where the loser dropped, `None` once they're eliminated.
        loser_to: Option<String>,
        /// Incomplete sets left in the bracket after this result.
        remaining: usize,
    },
}

/// The candidate the greedy ranker placed second when a call was committed.
#[derive(Debug, Clone, PartialEq)]
pub struct RunnerUp {
    pub bracket: BracketId,
    pub players: String,
    pub round_text: String,
    pub components: ScoreComponents,
}

/// A set's immediate bracket neighborhood — where each player came from and
/// where its winner/loser go. The replay's zoomed-in view of the part of the
/// bracket that changed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SetContext {
    /// One entry per occupied slot: "Alice ← won Winners Round 2 (C)".
    pub sources: Vec<String>,
    pub winner_to: Option<String>,
    pub loser_to: Option<String>,
}

pub fn simulate(world: &SimWorld, durations: &DurationModel) -> SimOutcome {
    simulate_inner(world, durations, None, false, false).0
}

pub fn simulate_action(world: &SimWorld, durations: &DurationModel, action: &Action) -> SimOutcome {
    simulate_inner(world, durations, Some(action), false, false).0
}

/// [`simulate`], additionally recording a [`ScriptFrame`] per completion (in
/// sim-time order; cascades yield several frames at one timestamp).
pub fn simulate_recorded(world: &SimWorld, durations: &DurationModel) -> (SimOutcome, Vec<ScriptFrame>) {
    let (outcome, frames, _) = simulate_inner(world, durations, None, true, false);
    (outcome, frames)
}

/// [`simulate`], additionally recording every greedy call (with score
/// ingredients) and every completion — the `--autoplay` replay material.
pub fn simulate_autoplay(world: &SimWorld, durations: &DurationModel) -> (SimOutcome, Vec<ReplayEvent>) {
    let (outcome, _, events) = simulate_inner(world, durations, None, false, true);
    (outcome, events)
}

fn simulate_inner(
    world: &SimWorld,
    durations: &DurationModel,
    action: Option<&Action>,
    record: bool,
    replay: bool,
) -> (SimOutcome, Vec<ScriptFrame>, Vec<ReplayEvent>) {
    let mut state = SimState::init(world, durations);
    if record {
        state.recorder = Some(Vec::new());
    }
    if replay {
        state.replay = Some(Vec::new());
    }
    if let Some(action) = action {
        state.apply_action(action);
    }
    loop {
        state.auto_complete_walkovers();
        while let Some(assignment) = state.next_assignment() {
            state.record_call(&assignment);
            state.assign(&assignment.bracket, &assignment.key, assignment.setup);
        }
        let Some(((t, _), event)) = state.events.pop_first() else {
            break;
        };
        state.clock = state.clock.max(t);
        state.hold_setup = None;
        if let Event::Complete { bracket, key } = event {
            state.apply_completion(&bracket, &key);
        }
    }
    let outcome = state.outcome(world);
    let frames = state.recorder.take().unwrap_or_default();
    let events = state.replay.take().unwrap_or_default();
    (outcome, frames, events)
}

#[derive(Debug, Clone)]
enum Event {
    Complete {
        bracket: BracketId,
        key: SetKey,
    },
    /// Re-run the assignment phase (bracket opens, rest expires, slots
    /// filled).
    Wake,
}

/// One committed greedy decision, with the runner-up it beat (replay only).
struct Assignment {
    bracket: BracketId,
    key: SetKey,
    setup: SetupId,
    components: ScoreComponents,
    runner_up: Option<(BracketId, SetKey, ScoreComponents)>,
}

struct SimState {
    brackets: Vec<SimBracket>,
    graphs: HashMap<BracketId, BracketGraph>,
    board: SetupBoard,
    /// `resting` stripped — the horizon models it instead.
    flags: PlayerFlags,
    tombstones: Tombstones,
    called_ints: Vec<i32>,
    aliases: AliasMap,
    soft_busy: Vec<(BracketId, SetKey)>,
    last_completed: HashMap<ConflictKey, UnixMillis>,
    rest_window_secs: u64,
    /// Originally-resting keys, busy until `rest_horizon_until`.
    resting: Vec<ConflictKey>,
    rest_horizon_until: UnixMillis,
    /// Fractional spread on per-set duration estimates (0 = smooth).
    duration_noise: f64,
    noise_seed: u64,
    durations: DurationModel,
    events: BTreeMap<(UnixMillis, u64), Event>,
    seq: u64,
    clock: UnixMillis,
    hold_setup: Option<SetupId>,
    finish_by_bracket: HashMap<BracketId, UnixMillis>,
    no_snoozes: HashMap<(BracketId, SetKey), UnixMillis>,
    no_callable_since: HashMap<SetKey, UnixMillis>,
    recorder: Option<Vec<ScriptFrame>>,
    replay: Option<Vec<ReplayEvent>>,
}

impl SimState {
    fn init(world: &SimWorld, durations: &DurationModel) -> Self {
        let mut flags = world.flags.clone();
        let mut resting: Vec<ConflictKey> = flags.resting.drain().collect();
        resting.sort();

        let mut state = Self {
            brackets: world.brackets.clone(),
            graphs: HashMap::new(),
            board: world.board.clone(),
            flags,
            tombstones: world.tombstones.clone(),
            called_ints: world.called_ints.clone(),
            aliases: world.aliases.clone(),
            soft_busy: world.soft_busy.clone(),
            last_completed: world.last_completed.clone(),
            rest_window_secs: world.rest_window_secs,
            resting,
            rest_horizon_until: world.now_millis + world.sim.rest_sim_horizon_secs as i64 * 1000,
            duration_noise: world.sim.duration_noise,
            noise_seed: world.sim.noise_seed,
            durations: durations.clone(),
            events: BTreeMap::new(),
            seq: 0,
            clock: world.now_millis,
            hold_setup: None,
            finish_by_bracket: HashMap::new(),
            no_snoozes: HashMap::new(),
            no_callable_since: HashMap::new(),
            recorder: None,
            replay: None,
        };
        for bracket in &state.brackets {
            let (graph, _) = BracketGraph::build(&bracket.sets, &bracket.groups);
            state.graphs.insert(bracket.id.clone(), graph);
        }

        let mut wakes = vec![state.rest_horizon_until];
        if world.rest_window_secs > 0 {
            let window = world.rest_window_secs as i64 * 1000;
            wakes.extend(world.last_completed.values().map(|&t| t + window).filter(|&e| e > state.clock));
        }
        wakes.extend(
            state
                .brackets
                .iter()
                .filter_map(|b| b.start_at.map(|s| s * 1000))
                .filter(|&open| open > state.clock),
        );
        for t in wakes {
            state.push_event(t, Event::Wake);
        }

        let completions = state.initial_in_flight();
        for (t, bracket, key) in completions {
            state.push_event(t, Event::Complete { bracket, key });
        }
        state
    }

    /// Resolves the snapshot's remote/local evidence into concrete finish
    /// events: awaiting-completion and locally-called sets finish one
    /// estimate from now; remotely-active sets at
    /// `max(startedAt + estimate, now + small remainder)`.
    fn initial_in_flight(&self) -> Vec<(UnixMillis, BracketId, SetKey)> {
        let board_linked: HashSet<(BracketId, SetKey)> = self
            .board
            .setups()
            .iter()
            .filter_map(|s| match &s.status {
                SetupStatus::Called { bracket, set } | SetupStatus::InProgress { bracket, set } => Some((bracket.clone(), set.clone())),
                SetupStatus::OccupiedExternal { set } => set.clone(),
                SetupStatus::Free => None,
            })
            .collect();

        let mut completions = Vec::new();
        for bracket in &self.brackets {
            for set in &bracket.sets {
                if set.is_completed() {
                    continue;
                }
                let at = (bracket.id.clone(), set.key.clone());
                let estimate = self.estimate_ms(bracket, &set.key);
                let finish = if self.tombstones.awaiting_remote_completion.contains(&at) {
                    Some(self.clock + estimate)
                } else if set.is_remotely_active() {
                    let started = set.started_at.expect("remotely active implies started");
                    Some((started * 1000 + estimate).max(self.clock + REMOTE_ACTIVE_MIN_REMAINDER_SECS * 1000))
                } else if set.called_evidence(&self.called_ints) || board_linked.contains(&at) {
                    Some(self.clock + estimate)
                } else {
                    None
                };
                if let Some(finish) = finish {
                    completions.push((finish, at.0, at.1));
                }
            }
        }
        completions
    }

    fn push_event(&mut self, t: UnixMillis, event: Event) {
        self.events.insert((t, self.seq), event);
        self.seq += 1;
    }

    fn bracket_index(&self, id: &BracketId) -> Option<usize> {
        self.brackets.iter().position(|b| &b.id == id)
    }

    fn estimate_ms(&self, bracket: &SimBracket, key: &SetKey) -> i64 {
        let best_of = bracket
            .groups
            .iter()
            .find(|g| g.id == key.phase_group)
            .and_then(|g| g.best_of_by_round.get(&key.round).copied());
        let base = self.durations.scaled_estimate_secs(&bracket.id, best_of) * 1000.0;
        (base * self.noise_factor(&bracket.id, key)) as i64
    }

    /// Seed-derived per-set duration multiplier in `1 ± duration_noise`; 1.0
    /// when noise is off. Fixed per (bracket, set) so re-sims and rollout
    /// action comparisons see the same world.
    fn noise_factor(&self, bracket: &BracketId, key: &SetKey) -> f64 {
        if self.duration_noise <= 0.0 {
            return 1.0;
        }
        let hash = fnv1a64(&[
            &self.noise_seed.to_le_bytes(),
            bracket.0.as_bytes(),
            key.phase_group.as_bytes(),
            &key.round.to_le_bytes(),
            key.identifier.as_bytes(),
        ]);
        let unit = (hash >> 11) as f64 / (1u64 << 53) as f64;
        1.0 + self.duration_noise * (unit * 2.0 - 1.0)
    }

    fn views(&self) -> Vec<BracketView<'_>> {
        self.brackets
            .iter()
            .map(|b| BracketView {
                id: &b.id,
                sets: &b.sets,
                mode: b.mode,
                start_at: b.start_at,
                held: b.held,
                pool: &b.pool,
            })
            .collect()
    }

    fn inputs(&self) -> ConflictInputs<'_> {
        ConflictInputs {
            aliases: &self.aliases,
            board: &self.board,
            flags: &self.flags,
            tombstones: &self.tombstones,
            called_ints: &self.called_ints,
            soft_busy: &self.soft_busy,
            last_completed: &self.last_completed,
            rest_window_secs: self.rest_window_secs,
            snoozes: &self.no_snoozes,
        }
    }

    /// The relaxed-regime index: real build plus the resting horizon.
    fn conflict_index(&self, views: &[BracketView<'_>], inputs: &ConflictInputs<'_>) -> ConflictIndex {
        let mut index = ConflictIndex::build(views, inputs);
        if self.rest_horizon_until > self.clock {
            for key in &self.resting {
                if !self.flags.force_available.contains(key) {
                    index.extend_rest(key.clone(), self.rest_horizon_until);
                }
            }
        }
        index
    }

    /// One greedy decision: the first free setup (board order, holds
    /// skipped) with a non-empty ranking takes its top candidate.
    fn next_assignment(&self) -> Option<Assignment> {
        let views = self.views();
        let inputs = self.inputs();
        let index = self.conflict_index(&views, &inputs);
        let candidates = callable_sets(&views, &index, &inputs, self.clock);
        if candidates.is_empty() {
            return None;
        }

        let graph_refs: Vec<_> = self.graphs.iter().collect();
        let remaining = aggregate_remaining(&graph_refs, &self.aliases);
        let ctx = RankContext {
            graphs: &self.graphs,
            remaining: &remaining,
            aliases: &self.aliases,
            callable_since: &self.no_callable_since,
            now_millis: self.clock,
        };

        for setup in self.board.free_ids() {
            if self.hold_setup == Some(setup) {
                continue;
            }
            let ranked = GreedyRanker.rank(setup, &candidates, &ctx);
            let Some(top) = ranked.first() else {
                continue;
            };
            let RankedAction::Call(callable) = &top.action else {
                continue;
            };
            let runner_up = ranked.get(1).and_then(|second| match &second.action {
                RankedAction::Call(c) => Some((c.bracket.clone(), c.key.clone(), second.components.clone())),
                RankedAction::Hold { .. } => None,
            });
            return Some(Assignment {
                bracket: callable.bracket.clone(),
                key: callable.key.clone(),
                setup,
                components: top.components.clone(),
                runner_up,
            });
        }
        None
    }

    /// "players — round" labels for a set, resolved from its bracket table.
    fn describe_set(&self, bracket_id: &BracketId, key: &SetKey) -> (String, String) {
        self.bracket_index(bracket_id)
            .and_then(|b| self.brackets[b].sets.iter().find(|s| &s.key == key))
            .map(|s| (join_players(s), s.full_round_text.clone().unwrap_or_default()))
            .unwrap_or_default()
    }

    /// Feeds the `--autoplay` replay log; a no-op unless armed.
    fn record_call(&mut self, assignment: &Assignment) {
        if self.replay.is_none() {
            return;
        }
        let Assignment {
            bracket: bracket_id,
            key,
            setup,
            components,
            runner_up,
        } = assignment;
        let Some(b) = self.bracket_index(bracket_id) else {
            return;
        };
        let est_finish = self.clock + self.estimate_ms(&self.brackets[b], key);
        let (players, round_text) = self.describe_set(bracket_id, key);
        let context = self.brackets[b]
            .sets
            .iter()
            .find(|s| &s.key == key)
            .map(|s| set_context(&self.brackets[b].sets, s))
            .unwrap_or_default();
        let runner_up = runner_up.as_ref().map(|(ru_bracket, ru_key, ru_components)| {
            let (ru_players, ru_round) = self.describe_set(ru_bracket, ru_key);
            RunnerUp {
                bracket: ru_bracket.clone(),
                players: ru_players,
                round_text: ru_round,
                components: ru_components.clone(),
            }
        });
        let event = ReplayEvent::Call {
            at: self.clock,
            setup: *setup,
            bracket: bracket_id.clone(),
            key: key.clone(),
            players,
            round_text,
            components: components.clone(),
            runner_up,
            context,
            est_finish,
        };
        if let Some(events) = &mut self.replay {
            events.push(event);
        }
    }

    /// Starts a set on a setup now (sim treats call→start as instant).
    fn assign(&mut self, bracket_id: &BracketId, key: &SetKey, setup: SetupId) {
        let Some(b) = self.bracket_index(bracket_id) else {
            return;
        };
        let finish = self.clock + self.estimate_ms(&self.brackets[b], key);
        let clock_secs = self.clock / 1000;
        if let Some(set) = self.brackets[b].sets.iter_mut().find(|s| &s.key == key) {
            set.started_at = Some(clock_secs);
        }
        self.board.set_status(
            setup,
            SetupStatus::InProgress {
                bracket: bracket_id.clone(),
                set: key.clone(),
            },
        );
        self.push_event(
            finish,
            Event::Complete {
                bracket: bracket_id.clone(),
                key: key.clone(),
            },
        );
    }

    fn apply_action(&mut self, action: &Action) {
        match action {
            Action::Call { bracket, set, setup } => self.assign(bracket, set, *setup),
            Action::Hold { setup } => self.hold_setup = Some(*setup),
        }
    }

    /// Zero-duration rule: a fully-resolved set with a DQ'd or departed
    /// occupant completes instantly (the healthy side advances), consuming
    /// no setup. Cascades until quiet.
    fn auto_complete_walkovers(&mut self) {
        while let Some((bracket, key)) = self.find_walkover() {
            self.apply_completion(&bracket, &key);
        }
    }

    fn find_walkover(&self) -> Option<(BracketId, SetKey)> {
        for bracket in &self.brackets {
            let open = bracket.mode == BracketMode::Full && !bracket.held && bracket.start_at.is_none_or(|s| s * 1000 <= self.clock);
            if !open {
                continue;
            }
            for set in &bracket.sets {
                if set.is_completed() || !set.all_slots_occupied() || is_reset_shaped(set) {
                    continue;
                }
                if set.occupants().any(|o| self.occupant_absent(o)) {
                    return Some((bracket.id.clone(), set.key.clone()));
                }
            }
        }
        None
    }

    fn occupant_absent(&self, occupant: &SlotOccupant) -> bool {
        occupant.is_disqualified
            || occupant_keys(occupant, &self.aliases)
                .iter()
                .any(|k| self.flags.departed.contains(k))
    }

    fn apply_completion(&mut self, bracket_id: &BracketId, key: &SetKey) {
        let clock = self.clock;
        let Some(b) = self.bracket_index(bracket_id) else {
            return;
        };
        let Some(pos) = self.brackets[b].sets.iter().position(|s| &s.key == key) else {
            return;
        };
        if self.brackets[b].sets[pos].is_completed() {
            return;
        }

        // Slot-0/higher-seed deterministic winner, skipping absentees.
        let winner_slot = self.brackets[b].sets[pos]
            .slots
            .iter()
            .position(|s| s.occupant.as_ref().is_some_and(|o| !self.occupant_absent(o)))
            .unwrap_or(0);

        let (completed_id, winner, loser, occupants) = {
            let set = &mut self.brackets[b].sets[pos];
            set.completed_at = Some(clock / 1000);
            let winner = set.slots.get(winner_slot).and_then(|s| s.occupant.clone());
            set.winner_id = winner.as_ref().map(|o| o.entrant_id.clone());
            let loser = set
                .slots
                .iter()
                .enumerate()
                .find(|(i, s)| *i != winner_slot && s.occupant.is_some())
                .and_then(|(_, s)| s.occupant.clone());
            let occupants: Vec<SlotOccupant> = set.occupants().cloned().collect();
            (set.id.clone(), winner, loser, occupants)
        };

        // Winner/loser propagation along prereq edges; reset-shaped sets
        // never fill (the sim's winners-side champion never fires a reset).
        for other in &mut self.brackets[b].sets {
            if other.is_completed() || is_reset_shaped(other) {
                continue;
            }
            for slot in &mut other.slots {
                let Some(Prereq::Set { id, placement }) = &slot.prereq else {
                    continue;
                };
                if id != &completed_id || slot.occupant.is_some() {
                    continue;
                }
                slot.occupant = match placement {
                    Some(2) => loser.clone(),
                    _ => winner.clone(),
                };
            }
            clear_resolved_placeholder(other);
        }

        if self.brackets[b].mode == BracketMode::Full {
            let entry = self.finish_by_bracket.entry(bracket_id.clone()).or_insert(clock);
            *entry = (*entry).max(clock);
        }

        // Free whatever station was running it.
        let freed: Vec<SetupId> = self
            .board
            .setups()
            .iter()
            .filter(|s| match &s.status {
                SetupStatus::Called { bracket, set } | SetupStatus::InProgress { bracket, set } => bracket == bracket_id && set == key,
                SetupStatus::OccupiedExternal {
                    set: Some((linked, linked_key)),
                } => linked == bracket_id && linked_key == key,
                _ => false,
            })
            .map(|s| s.id)
            .collect();
        for setup in freed {
            self.board.set_status(setup, SetupStatus::Free);
        }

        // Rest windows: completion time per conflict key, plus a wake at
        // expiry so the assignment phase re-runs.
        if self.rest_window_secs > 0 {
            for occupant in &occupants {
                for k in occupant_keys(occupant, &self.aliases) {
                    let entry = self.last_completed.entry(k).or_insert(clock);
                    *entry = (*entry).max(clock);
                }
            }
            self.push_event(clock + self.rest_window_secs as i64 * 1000, Event::Wake);
        }

        let synthesized = self.synthesize_due_swiss_rounds(b);
        let filled = self.fill_group_progressions(b);
        if synthesized || filled {
            self.push_event(clock, Event::Wake);
        }
        let bracket = &self.brackets[b];
        let (graph, _) = BracketGraph::build(&bracket.sets, &bracket.groups);
        self.graphs.insert(bracket_id.clone(), graph);

        if let Some(frames) = &mut self.recorder {
            frames.push(ScriptFrame {
                at: clock,
                bracket: bracket_id.clone(),
                sets: self.brackets[b].sets.clone(),
            });
        }
        if self.replay.is_some() {
            let sets = &self.brackets[b].sets;
            let remaining = sets.iter().filter(|s| !s.is_completed()).count();
            let players = occupants.iter().map(|o| o.display_name.as_str()).collect::<Vec<_>>().join(" vs ");
            // Post-propagation, so destination labels carry the opponent
            // already waiting there.
            let context = sets
                .iter()
                .find(|s| &s.key == key)
                .map(|s| set_context(sets, s))
                .unwrap_or_default();
            let event = ReplayEvent::Complete {
                at: clock,
                bracket: bracket_id.clone(),
                key: key.clone(),
                players,
                winner: winner.map(|o| o.display_name).unwrap_or_default(),
                winner_to: context.winner_to,
                loser_to: context.loser_to,
                remaining,
            };
            if let Some(events) = &mut self.replay {
                events.push(event);
            }
        }
    }

    /// Live swiss only materializes the current round; once it fully
    /// completes, synthesize the next one by pairing active entrants in
    /// deterministic order.
    fn synthesize_due_swiss_rounds(&mut self, b: usize) -> bool {
        let bracket = &self.brackets[b];
        let mut new_sets = Vec::new();
        for group in &bracket.groups {
            let GroupKind::Swiss { num_rounds } = group.kind else {
                continue;
            };
            let group_sets: Vec<&LiveSet> = bracket.sets.iter().filter(|s| s.key.phase_group == group.id).collect();
            let Some(current) = group_sets.iter().map(|s| s.key.round).max() else {
                continue;
            };
            if current >= num_rounds || group_sets.iter().any(|s| !s.is_completed()) {
                continue;
            }

            let mut entrants = distinct_occupants(&group_sets);
            entrants.retain(|o| !self.occupant_absent(o));
            let round = current + 1;
            for (i, pair) in entrants.chunks(2).enumerate() {
                let [a, second] = pair else {
                    continue; // odd player out: bye round
                };
                new_sets.push(synthesized_set(&group.id, round, i, a.clone(), second.clone()));
            }
        }
        let changed = !new_sets.is_empty();
        self.brackets[b].sets.extend(new_sets);
        changed
    }

    /// Cross-group progression (pools → DE, swiss → cut): when a group
    /// finishes, its qualifiers (wins desc, entrant id asc) fill the next
    /// group's still-empty seed slots.
    fn fill_group_progressions(&mut self, b: usize) -> bool {
        let mut changed = false;
        for g in 1..self.brackets[b].groups.len() {
            let bracket = &self.brackets[b];
            let (prev, cur) = (&bracket.groups[g - 1], &bracket.groups[g]);
            if !group_finished(&bracket.sets, prev) {
                continue;
            }

            let placed: HashSet<EntrantId> = bracket
                .sets
                .iter()
                .filter(|s| s.key.phase_group == cur.id)
                .flat_map(|s| s.occupants())
                .map(|o| o.entrant_id.clone())
                .collect();

            let prev_sets: Vec<&LiveSet> = bracket.sets.iter().filter(|s| s.key.phase_group == prev.id).collect();
            let mut wins: HashMap<EntrantId, u32> = HashMap::new();
            for set in &prev_sets {
                if let Some(winner) = &set.winner_id {
                    *wins.entry(winner.clone()).or_insert(0) += 1;
                }
            }
            let mut qualifiers = distinct_occupants(&prev_sets);
            qualifiers.retain(|o| !self.occupant_absent(o) && !placed.contains(&o.entrant_id));
            qualifiers.sort_by(|x, y| {
                let (wx, wy) = (
                    wins.get(&x.entrant_id).copied().unwrap_or(0),
                    wins.get(&y.entrant_id).copied().unwrap_or(0),
                );
                wy.cmp(&wx).then_with(|| x.entrant_id.cmp(&y.entrant_id))
            });

            let cur_id = cur.id.clone();
            let mut next_qualifier = qualifiers.into_iter();
            'fill: for set in self.brackets[b]
                .sets
                .iter_mut()
                .filter(|s| s.key.phase_group == cur_id && !s.is_completed())
            {
                for slot in &mut set.slots {
                    let pending = slot.occupant.is_none() && !matches!(&slot.prereq, Some(Prereq::Set { .. }));
                    if !pending {
                        continue;
                    }
                    let Some(qualifier) = next_qualifier.next() else {
                        break 'fill;
                    };
                    slot.occupant = Some(qualifier);
                    changed = true;
                }
                clear_resolved_placeholder(set);
            }
        }
        changed
    }

    fn outcome(&self, world: &SimWorld) -> SimOutcome {
        let mut per_bracket_finish = HashMap::new();
        let mut blocked = Vec::new();
        for bracket in &self.brackets {
            if bracket.mode != BracketMode::Full {
                continue;
            }
            let graph = &self.graphs[&bracket.id];
            let unfinished = bracket
                .sets
                .iter()
                .enumerate()
                .any(|(i, s)| !s.is_completed() && !graph.is_gf_reset_excluded(i));
            if unfinished {
                blocked.push(bracket.id.clone());
            } else {
                per_bracket_finish.insert(
                    bracket.id.clone(),
                    self.finish_by_bracket.get(&bracket.id).copied().unwrap_or(world.now_millis),
                );
            }
        }

        let includes_unstarted = world
            .brackets
            .iter()
            .filter(|b| b.mode == BracketMode::Full && b.start_at.is_some_and(|s| s * 1000 > world.now_millis))
            .map(|b| b.id.clone())
            .collect();

        SimOutcome {
            overall_finish: per_bracket_finish.values().copied().max().unwrap_or(world.now_millis),
            per_bracket_finish,
            blocked,
            includes_unstarted,
        }
    }
}

/// "A vs B" from a set's occupants (replay labels).
fn join_players(set: &LiveSet) -> String {
    set.occupants().map(|o| o.display_name.as_str()).collect::<Vec<_>>().join(" vs ")
}

/// Builds a set's [`SetContext`] from its bracket table: slot provenance via
/// the prereq edges in, winner/loser destinations via the edges out.
fn set_context(sets: &[LiveSet], set: &LiveSet) -> SetContext {
    let sources = set
        .slots
        .iter()
        .filter_map(|slot| {
            let name = &slot.occupant.as_ref()?.display_name;
            let origin = match &slot.prereq {
                Some(Prereq::Set { id, placement }) => sets.iter().find(|s| &s.id == id).map(|feeder| {
                    let verb = if *placement == Some(2) { "lost" } else { "won" };
                    format!("{verb} {}", set_label(feeder))
                }),
                _ => None,
            };
            Some(format!("{name} ← {}", origin.unwrap_or_else(|| "seed".to_owned())))
        })
        .collect();

    let mut winner_to = None;
    let mut loser_to = None;
    for dest in sets {
        if dest.is_completed() {
            continue;
        }
        for (i, slot) in dest.slots.iter().enumerate() {
            let Some(Prereq::Set { id, placement }) = &slot.prereq else {
                continue;
            };
            if id != &set.id {
                continue;
            }
            let opponent = dest
                .slots
                .iter()
                .enumerate()
                .find_map(|(j, s)| (j != i).then_some(s.occupant.as_ref()).flatten())
                .map(|o| o.display_name.clone());
            let label = match opponent {
                Some(opponent) => format!("{} vs {opponent}", set_label(dest)),
                None => set_label(dest),
            };
            match placement {
                Some(2) => loser_to = Some(label),
                _ => winner_to = Some(label),
            }
        }
    }
    SetContext {
        sources,
        winner_to,
        loser_to,
    }
}

/// "Winners Final (A)": round text plus the set identifier, or just the
/// identifier when the API sent no round text.
fn set_label(set: &LiveSet) -> String {
    match set.full_round_text.as_deref() {
        Some(text) if !text.is_empty() => format!("{text} ({})", set.key.identifier),
        _ => format!("set {}", set.key.identifier),
    }
}

/// FNV-1a over the concatenated chunks: a stable, dependency-free hash so
/// duration noise reproduces across runs and builds for the same seed.
fn fnv1a64(chunks: &[&[u8]]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for chunk in chunks {
        for &byte in *chunk {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    hash
}

/// The sim stands in for the server when it fills slots, so it also clears
/// `hasPlaceholder` the way the server does once a set's slots resolve —
/// otherwise a placeholder-flagged cut is never callable and the projection
/// starves.
fn clear_resolved_placeholder(set: &mut LiveSet) {
    if set.has_placeholder && set.all_slots_occupied() {
        set.has_placeholder = false;
    }
}

/// Both slots prereq the same set: the GF-reset shape. The sim's
/// winners-side champion never fires it.
fn is_reset_shaped(set: &LiveSet) -> bool {
    let ids: Vec<_> = set
        .slots
        .iter()
        .filter_map(|s| match &s.prereq {
            Some(Prereq::Set { id, .. }) => Some(id),
            _ => None,
        })
        .collect();
    matches!(ids.as_slice(), [a, b] if a == b)
}

fn group_finished(sets: &[LiveSet], group: &PhaseGroupInfo) -> bool {
    let group_sets: Vec<&LiveSet> = sets.iter().filter(|s| s.key.phase_group == group.id).collect();
    if group_sets.is_empty() || group_sets.iter().any(|s| !s.is_completed()) {
        return false;
    }
    match group.kind {
        GroupKind::Swiss { num_rounds } => group_sets.iter().map(|s| s.key.round).max().unwrap_or(0) >= num_rounds,
        _ => true,
    }
}

/// Distinct occupants across sets, ordered by entrant id (deterministic).
fn distinct_occupants(sets: &[&LiveSet]) -> Vec<SlotOccupant> {
    let mut by_entrant: BTreeMap<EntrantId, SlotOccupant> = BTreeMap::new();
    for set in sets {
        for occupant in set.occupants() {
            by_entrant.entry(occupant.entrant_id.clone()).or_insert_with(|| occupant.clone());
        }
    }
    by_entrant.into_values().collect()
}

fn synthesized_set(pg: &str, round: i32, idx: usize, a: SlotOccupant, b: SlotOccupant) -> LiveSet {
    let slot = |occupant| Slot {
        prereq: None,
        occupant: Some(occupant),
    };
    LiveSet {
        id: SetId(format!("sim_{pg}_{round}_{idx}")),
        key: SetKey {
            phase_group: pg.to_owned(),
            round,
            identifier: format!("S{round}-{idx}"),
        },
        state_int: None,
        full_round_text: Some(format!("Round {round} (projected)")),
        started_at: None,
        completed_at: None,
        winner_id: None,
        has_placeholder: false,
        slots: vec![slot(a), slot(b)],
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        time::{Duration, Instant},
    };

    use super::{simulate, simulate_action, simulate_recorded, Action, SimBracket, SimWorld};
    use crate::{
        config::{BracketMode, SetupId, SimConfig},
        conflict::{AliasMap, ConflictKey, PlayerFlags, SetupBoard, SetupStatus, Tombstones},
        duration::DurationModel,
        model::{BracketId, PlayerId},
        synth::{make_de_bracket, make_fbr_world, make_swiss, make_unseeded_se, SynthBracket},
    };

    /// now = a round number so hand-computed finishes stay readable.
    const NOW: i64 = 1_751_000_000_000;
    /// Default prior: 480s per bo3 set.
    const SET_MS: i64 = 480_000;

    fn full_bracket(id: &str, bracket: SynthBracket, pool: &[SetupId]) -> SimBracket {
        SimBracket {
            id: BracketId(id.to_owned()),
            sets: bracket.sets,
            groups: vec![bracket.info],
            mode: BracketMode::Full,
            start_at: None,
            held: false,
            pool: pool.to_vec(),
        }
    }

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

    fn de4_world() -> SimWorld {
        let setups = [SetupId(1), SetupId(2)];
        world(vec![full_bracket("melee", make_de_bracket(9, 4), &setups)], &setups)
    }

    #[test]
    fn hand_computed_makespan_two_setups_de4() {
        // t0: W1A + W1B on the two setups. t480: W2 + L1. t960: L2.
        // t1440: GF. t1920: done; reset pruned.
        let outcome = simulate(&de4_world(), &DurationModel::new());
        assert_eq!(outcome.overall_finish, NOW + 4 * SET_MS);
        assert_eq!(outcome.per_bracket_finish[&BracketId("melee".to_owned())], NOW + 4 * SET_MS);
        assert!(outcome.blocked.is_empty());
        assert!(outcome.includes_unstarted.is_empty());
    }

    #[test]
    fn single_setup_serializes_fully() {
        let setups = [SetupId(1)];
        let world = world(vec![full_bracket("melee", make_de_bracket(9, 4), &setups)], &setups);
        // 6 real sets (reset never fires) end to end on one station.
        let outcome = simulate(&world, &DurationModel::new());
        assert_eq!(outcome.overall_finish, NOW + 6 * SET_MS);
    }

    #[test]
    fn no_progress_guard_blocks_starved_brackets() {
        // The only permitted setup is externally occupied with no tracked
        // set: nothing ever frees it.
        let mut w = de4_world();
        w.board.set_status(SetupId(1), SetupStatus::OccupiedExternal { set: None });
        w.board.set_status(SetupId(2), SetupStatus::OccupiedExternal { set: None });
        let outcome = simulate(&w, &DurationModel::new());
        assert_eq!(outcome.blocked, vec![BracketId("melee".to_owned())]);
        assert!(outcome.per_bracket_finish.is_empty());
        assert_eq!(outcome.overall_finish, NOW, "no projection when nothing can run");
    }

    #[test]
    fn no_progress_guard_untracked_pending_slots() {
        // A lone top cut waiting on standings that never arrive.
        let setups = [SetupId(1)];
        let world = world(vec![full_bracket("cut", make_unseeded_se(9, 4), &setups)], &setups);
        let outcome = simulate(&world, &DurationModel::new());
        assert_eq!(outcome.blocked, vec![BracketId("cut".to_owned())]);
    }

    #[test]
    fn unstarted_bracket_demand_included_and_labeled() {
        let mut w = de4_world();
        w.brackets[0].start_at = Some(NOW / 1000 + 3600);
        let outcome = simulate(&w, &DurationModel::new());
        assert_eq!(outcome.includes_unstarted, vec![BracketId("melee".to_owned())]);
        assert_eq!(
            outcome.overall_finish,
            NOW + 3_600_000 + 4 * SET_MS,
            "waits for the bracket to open"
        );
    }

    #[test]
    fn departed_players_walk_over_at_zero_duration() {
        let mut w = de4_world();
        for p in ["P3", "P4"] {
            w.flags.departed.insert(ConflictKey::Player(PlayerId(p.to_owned())));
        }
        // W1s + L1 walk over instantly; only W2 (P1 vs P2) and GF actually
        // run: L2's absentee side auto-completes when W2's loser drops in.
        let outcome = simulate(&w, &DurationModel::new());
        assert_eq!(outcome.overall_finish, NOW + 2 * SET_MS);

        // And nothing walked over consumed a station: with one setup the
        // answer is identical.
        let setups = [SetupId(1)];
        let mut w = world(vec![full_bracket("melee", make_de_bracket(9, 4), &setups)], &setups);
        for p in ["P3", "P4"] {
            w.flags.departed.insert(ConflictKey::Player(PlayerId(p.to_owned())));
        }
        let outcome = simulate(&w, &DurationModel::new());
        assert_eq!(outcome.overall_finish, NOW + 2 * SET_MS);
    }

    #[test]
    fn remote_active_and_called_evidence_seed_in_flight_work() {
        let mut w = de4_world();
        // W1A started 10 minutes ago: finishes at the small-remainder floor,
        // not in the past.
        w.brackets[0].sets[0].started_at = Some(NOW / 1000 - 600);
        // W1B was called (state 6): starts now, full estimate.
        w.brackets[0].sets[1].state_int = Some(6);

        let outcome = simulate(&w, &DurationModel::new());
        // W1A at NOW+60s, W1B at NOW+480s; W2+L1 at 960s, L2 at 1440s,
        // GF at 1920s — same as calling both at t0 (the in-flight head start
        // is swallowed by the W1B estimate) but the path exercises both
        // evidence kinds.
        assert_eq!(outcome.overall_finish, NOW + 4 * SET_MS);
        assert!(outcome.blocked.is_empty());
    }

    #[test]
    fn awaiting_remote_completion_is_in_flight_not_callable() {
        let mut w = de4_world();
        w.tombstones
            .awaiting_remote_completion
            .insert((BracketId("melee".to_owned()), w.brackets[0].sets[0].key.clone()));
        let outcome = simulate(&w, &DurationModel::new());
        // The awaiting set completes at one estimate; everything else flows.
        assert_eq!(outcome.overall_finish, NOW + 4 * SET_MS);
        assert!(outcome.blocked.is_empty());
    }

    #[test]
    fn resting_players_return_after_horizon() {
        let mut w = de4_world();
        w.flags.resting.insert(ConflictKey::Player(PlayerId("P1".to_owned())));
        // Horizon default 600s: W1A (P1's set) can't start until NOW+600s.
        // W1B runs immediately. Critical path: W1A at 600..1080, W2
        // 1080..1560, L2 1560..2040, GF 2040..2520.
        let outcome = simulate(&w, &DurationModel::new());
        assert_eq!(outcome.overall_finish, NOW + 600_000 + 4 * SET_MS);
        assert!(outcome.blocked.is_empty());
    }

    #[test]
    fn rest_window_delays_follow_up_sets() {
        let setups = [SetupId(1), SetupId(2)];
        let mut w = world(vec![full_bracket("melee", make_de_bracket(9, 4), &setups)], &setups);
        w.rest_window_secs = 300;
        // Every completion imposes 300s of rest before the players' next
        // set: rounds space out to 480+300 apart after the first.
        let outcome = simulate(&w, &DurationModel::new());
        assert_eq!(outcome.overall_finish, NOW + 4 * SET_MS + 3 * 300_000);
    }

    #[test]
    fn swiss_synthesizes_future_rounds_to_completion() {
        let setups = [SetupId(1), SetupId(2)];
        let world = world(vec![full_bracket("pokemon", make_swiss(9, 9, 4), &setups)], &setups);
        // 4 rounds × 4 sets on 2 stations with a round barrier: 4 × 960s.
        let outcome = simulate(&world, &DurationModel::new());
        assert_eq!(outcome.overall_finish, NOW + 4 * 2 * SET_MS);
        assert!(outcome.blocked.is_empty());
    }

    #[test]
    fn swiss_feeds_top_cut_through_progression_fill() {
        let setups = [SetupId(1), SetupId(2)];
        let swiss = make_swiss(9, 8, 2);
        let cut = make_unseeded_se(10, 4);
        let bracket = SimBracket {
            id: BracketId("pokemon".to_owned()),
            sets: swiss.sets.into_iter().chain(cut.sets).collect(),
            groups: vec![swiss.info, cut.info],
            mode: BracketMode::Full,
            start_at: None,
            held: false,
            pool: setups.to_vec(),
        };
        let outcome = simulate(&world(vec![bracket], &setups), &DurationModel::new());
        // Swiss: 2 rounds × (4 sets / 2 setups) = 2×960s; cut: semis 480 +
        // final 480.
        assert_eq!(outcome.overall_finish, NOW + 2 * 2 * SET_MS + 2 * SET_MS);
        assert!(outcome.blocked.is_empty());
    }

    /// Live cut sets carry `hasPlaceholder` until the server fills their
    /// slots (observed on the FBR Pokemon capture). When the sim's own fills
    /// resolve the slots it must clear the flag too, or the cut is never
    /// callable and the projection starves.
    #[test]
    fn placeholder_flags_clear_when_the_sim_fills_slots() {
        let setups = [SetupId(1), SetupId(2)];
        let swiss = make_swiss(9, 8, 2);
        let mut cut = make_unseeded_se(10, 4);
        for set in &mut cut.sets {
            set.has_placeholder = true;
        }
        let bracket = SimBracket {
            id: BracketId("pokemon".to_owned()),
            sets: swiss.sets.into_iter().chain(cut.sets).collect(),
            groups: vec![swiss.info, cut.info],
            mode: BracketMode::Full,
            start_at: None,
            held: false,
            pool: setups.to_vec(),
        };
        let outcome = simulate(&world(vec![bracket], &setups), &DurationModel::new());
        assert!(outcome.blocked.is_empty(), "placeholder cut starved: {:?}", outcome.blocked);
        assert_eq!(outcome.overall_finish, NOW + 2 * 2 * SET_MS + 2 * SET_MS);
    }

    #[test]
    fn hold_defers_one_setup_until_next_event() {
        let mut w = de4_world();
        // One set is already running remotely; hold setup 1 and compare.
        w.brackets[0].sets[0].started_at = Some(NOW / 1000 - 60);

        let baseline = simulate(&w, &DurationModel::new());
        let held = simulate_action(&w, &DurationModel::new(), &Action::Hold { setup: SetupId(1) });
        // Holding costs nothing here: W1B just starts on setup 2 instead.
        assert_eq!(baseline.overall_finish, held.overall_finish);
        assert!(held.blocked.is_empty());
    }

    #[test]
    fn forced_call_is_the_rollout_seam() {
        let w = de4_world();
        let key_b = w.brackets[0].sets[1].key.clone();
        let outcome = simulate_action(
            &w,
            &DurationModel::new(),
            &Action::Call {
                bracket: BracketId("melee".to_owned()),
                set: key_b,
                setup: SetupId(2),
            },
        );
        // Same makespan as greedy (symmetric world), just a forced first
        // move.
        assert_eq!(outcome.overall_finish, NOW + 4 * SET_MS);
    }

    #[test]
    fn conflict_only_brackets_gate_shared_players_but_never_finish() {
        let setups = [SetupId(1), SetupId(2)];
        let mut ladder = full_bracket("ladder", make_de_bracket(30, 4), &setups);
        ladder.mode = BracketMode::ConflictOnly;
        // The ladder's W1A (P1, P4) is running remotely for 480s more.
        ladder.sets[0].started_at = Some(NOW / 1000);

        let melee = full_bracket("melee", make_de_bracket(9, 4), &setups);
        let outcome = simulate(&world(vec![ladder, melee], &setups), &DurationModel::new());

        // Melee W1A shares P1/P4 with the ladder set, so it waits 480s, then
        // the rest of the chain runs W2 at 960, L2 at 1440, GF at 1920.
        assert_eq!(outcome.overall_finish, NOW + 5 * SET_MS);
        assert_eq!(outcome.per_bracket_finish.len(), 1, "conflict-only brackets have no finish entry");
        assert!(outcome.blocked.is_empty(), "conflict-only brackets are never 'blocked'");
    }

    #[test]
    fn recorded_frames_replay_to_full_completion() {
        let (outcome, frames) = simulate_recorded(&de4_world(), &DurationModel::new());
        assert_eq!(
            outcome,
            simulate(&de4_world(), &DurationModel::new()),
            "recording never changes the outcome"
        );

        // 6 real sets complete (the reset never fires), in sim-time order.
        assert_eq!(frames.len(), 6);
        assert!(frames.windows(2).all(|w| w[0].at <= w[1].at));
        let last = frames.last().unwrap();
        assert_eq!(last.at, outcome.overall_finish);
        assert_eq!(last.sets.iter().filter(|s| s.is_completed()).count(), 6);
    }

    #[test]
    fn recorded_frames_carry_the_changed_brackets_full_table() {
        let setups = [SetupId(1), SetupId(2)];
        let w = world(
            vec![
                full_bracket("melee", make_de_bracket(9, 4), &setups),
                full_bracket("ult", make_de_bracket(30, 4), &setups),
            ],
            &setups,
        );
        let (outcome, frames) = simulate_recorded(&w, &DurationModel::new());

        assert!(outcome.blocked.is_empty());
        for frame in &frames {
            let source = w.brackets.iter().find(|b| b.id == frame.bracket).unwrap();
            assert_eq!(frame.sets.len(), source.sets.len(), "a frame is the whole bracket table");
        }
        for bracket in ["melee", "ult"] {
            let id = BracketId(bracket.to_owned());
            assert!(frames.iter().any(|f| f.bracket == id), "{bracket} never recorded");
        }
    }

    #[test]
    fn determinism_identical_worlds_identical_outcomes() {
        let a = simulate(&de4_world(), &DurationModel::new());
        let b = simulate(&de4_world(), &DurationModel::new());
        assert_eq!(a, b);
    }

    /// The plan's perf tripwire: a full FBR-shaped Friday (7 events, ~315
    /// sets, heavy player overlap) must simulate end-to-end well inside a
    /// generous debug-build bound.
    #[test]
    fn perf_full_pipeline_on_fbr_world() {
        let started = Instant::now();
        let setups: Vec<SetupId> = (1..=8).map(SetupId).collect();
        let brackets: Vec<SimBracket> = make_fbr_world()
            .into_iter()
            .map(|event| SimBracket {
                id: event.id,
                sets: event.sets,
                groups: event.groups,
                mode: BracketMode::Full,
                start_at: None,
                held: false,
                pool: setups.clone(),
            })
            .collect();
        let outcome = simulate(&world(brackets, &setups), &DurationModel::new());

        assert!(outcome.blocked.is_empty(), "blocked: {:?}", outcome.blocked);
        assert_eq!(outcome.per_bracket_finish.len(), 7);
        assert!(outcome.overall_finish > NOW);
        let elapsed = started.elapsed();
        assert!(elapsed < Duration::from_secs(30), "took {elapsed:?}");
    }
}

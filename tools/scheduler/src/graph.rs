//! Per-bracket structure, rebuilt from scratch on every poll snapshot.
//!
//! The graph branches per PHASE GROUP on the structure query's bracket type
//! (mixed-type events are the norm live: RR pools → DE, swiss → top cut), not
//! on config expectations. All findings are returned as deduplicated
//! [`GraphWarning`]s — the pure core never logs.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::model::{EntrantId, GroupKind, LiveSet, PhaseGroupInfo, PlayerId, Prereq, SetId, SetKey};

/// A downstream edge: `set` takes this feeder's winner (`placement` 1) or
/// loser (`placement` 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dependent {
    pub set: usize,
    pub placement: Option<i32>,
}

/// Non-fatal structural findings, deduplicated.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum GraphWarning {
    /// A `prereqType == "set"` edge references a set the API never returned
    /// (bye-degenerate sets; permanent, not just pre-start). The slot is
    /// treated as pre-satisfied.
    DanglingPrereq { referenced: SetId },
    /// A set's phase group is missing from the structure info; its sets are
    /// treated as elimination.
    UnknownPhaseGroup { phase_group: String },
    /// A prereq cycle (corrupt data); the back edge contributes no depth.
    CycleDetected { at: SetKey },
}

/// Per-phase-group rollup computed at build time.
#[derive(Debug, Clone)]
pub struct GroupStats {
    pub info: PhaseGroupInfo,
    pub set_indices: Vec<usize>,
    /// Distinct entrants observed in the group's sets, sorted.
    pub active_entrants: Vec<EntrantId>,
    /// How many sequential sets this group still needs (kind-specific; the
    /// bracket's critical path sums these across sequential stages).
    pub remaining_depth: u32,
    /// Swiss only: how many future-round sets to synthesize for projections
    /// (`rounds_remaining × floor(active_entrants / 2)`).
    pub swiss_future_demand: u32,
}

/// One bracket's (event's) structure for a single snapshot: index maps,
/// downstream adjacency, per-set depth metrics, and per-group rollups.
#[derive(Debug, Clone)]
pub struct BracketGraph {
    sets: Vec<LiveSet>,
    by_id: HashMap<SetId, usize>,
    by_key: HashMap<SetKey, usize>,
    dependents: Vec<Vec<Dependent>>,
    depth: Vec<u32>,
    unblock: Vec<u32>,
    gf_reset_excluded: Vec<bool>,
    groups: Vec<GroupStats>,
    group_index: HashMap<String, usize>,
    remaining_by_player: HashMap<PlayerId, u32>,
    remaining_by_entrant: HashMap<EntrantId, u32>,
    remaining_critical_path: u32,
}

impl BracketGraph {
    pub fn build(sets: &[LiveSet], groups: &[PhaseGroupInfo]) -> (Self, Vec<GraphWarning>) {
        let mut warnings = BTreeSet::new();
        let sets = sets.to_vec();

        let by_id: HashMap<_, _> = sets.iter().enumerate().map(|(i, s)| (s.id.clone(), i)).collect();
        let by_key: HashMap<_, _> = sets.iter().enumerate().map(|(i, s)| (s.key.clone(), i)).collect();

        let dependents = build_dependents(&sets, &by_id, &mut warnings);
        let gf_reset_excluded = gf_reset_exclusions(&sets, &by_id);
        let (groups, group_index) = group_stats_skeleton(&sets, groups);
        for set in &sets {
            if !group_index.contains_key(&set.key.phase_group) {
                warnings.insert(GraphWarning::UnknownPhaseGroup {
                    phase_group: set.key.phase_group.clone(),
                });
            }
        }

        let mut graph = Self {
            depth: vec![0; sets.len()],
            unblock: vec![0; sets.len()],
            remaining_by_player: HashMap::new(),
            remaining_by_entrant: HashMap::new(),
            remaining_critical_path: 0,
            sets,
            by_id,
            by_key,
            dependents,
            gf_reset_excluded,
            groups,
            group_index,
        };
        graph.compute_depths(&mut warnings);
        graph.compute_unblocks();
        graph.compute_remaining_counts();
        graph.compute_group_rollups();
        (graph, warnings.into_iter().collect())
    }

    pub fn sets(&self) -> &[LiveSet] {
        &self.sets
    }

    pub fn index_of_id(&self, id: &SetId) -> Option<usize> {
        self.by_id.get(id).copied()
    }

    pub fn index_of_key(&self, key: &SetKey) -> Option<usize> {
        self.by_key.get(key).copied()
    }

    /// Longest count of not-yet-completed sets on any downstream path,
    /// including this set itself. Completed sets pass chains through without
    /// counting; a GF-reset set contributes nothing until reachable.
    pub fn depth(&self, idx: usize) -> u32 {
        self.depth[idx]
    }

    /// How many incomplete sets this one directly feeds.
    pub fn unblock_count(&self, idx: usize) -> u32 {
        self.unblock[idx]
    }

    /// True for a set both of whose slots prereq the same undecided set (the
    /// GF-reset shape) while it isn't confirmed to happen.
    pub fn is_gf_reset_excluded(&self, idx: usize) -> bool {
        self.gf_reset_excluded[idx]
    }

    pub fn dependents(&self, idx: usize) -> &[Dependent] {
        &self.dependents[idx]
    }

    pub fn groups(&self) -> &[GroupStats] {
        &self.groups
    }

    pub fn group(&self, phase_group: &str) -> Option<&GroupStats> {
        self.group_index.get(phase_group).map(|&i| &self.groups[i])
    }

    /// Sequential sets remaining for the whole bracket: consecutive
    /// same-kind phase groups count as one parallel stage (pools run
    /// side-by-side), and stages sum.
    pub fn remaining_critical_path(&self) -> u32 {
        self.remaining_critical_path
    }

    /// Remaining incomplete sets this player occupies here, plus assumed
    /// future swiss rounds. Unmerged; alias merging is the conflict layer's.
    pub fn remaining_for_player(&self, player: &PlayerId) -> u32 {
        self.remaining_by_player.get(player).copied().unwrap_or(0)
    }

    /// Fallback count for identity-degraded occupants.
    pub fn remaining_for_entrant(&self, entrant: &EntrantId) -> u32 {
        self.remaining_by_entrant.get(entrant).copied().unwrap_or(0)
    }

    pub fn remaining_player_counts(&self) -> impl Iterator<Item = (&PlayerId, u32)> {
        self.remaining_by_player.iter().map(|(p, &n)| (p, n))
    }

    pub fn remaining_entrant_counts(&self) -> impl Iterator<Item = (&EntrantId, u32)> {
        self.remaining_by_entrant.iter().map(|(e, &n)| (e, n))
    }

    fn kind_of(&self, set_idx: usize) -> &GroupKind {
        self.group(&self.sets[set_idx].key.phase_group)
            .map_or(&GroupKind::Elimination, |g| &g.info.kind)
    }

    fn compute_depths(&mut self, warnings: &mut BTreeSet<GraphWarning>) {
        let kinds: Vec<GroupKind> = (0..self.sets.len()).map(|idx| self.kind_of(idx).clone()).collect();
        let rr_remaining = self.rr_remaining_by_entrant();

        let mut colors = vec![Color::Unvisited; self.sets.len()];
        for (idx, kind) in kinds.iter().enumerate() {
            self.depth[idx] = match kind {
                GroupKind::Elimination => elimination_depth(
                    idx,
                    &self.sets,
                    &self.dependents,
                    &self.gf_reset_excluded,
                    &mut colors,
                    &mut self.depth,
                    warnings,
                ),
                GroupKind::RoundRobin => self.rr_depth(idx, &rr_remaining),
                GroupKind::Swiss { num_rounds } => self.swiss_depth(idx, *num_rounds),
                GroupKind::Unsupported(_) => u32::from(!self.sets[idx].is_completed()),
            };
        }
        for idx in 0..self.sets.len() {
            if self.gf_reset_excluded[idx] {
                self.depth[idx] = 0;
            }
        }
    }

    /// RR: a set is as deep as its busiest occupant's remaining pool schedule
    /// (their sets serialize on the player, not on bracket edges).
    fn rr_depth(&self, idx: usize, rr_remaining: &HashMap<(String, EntrantId), u32>) -> u32 {
        let set = &self.sets[idx];
        if set.is_completed() {
            return 0;
        }
        set.occupants()
            .filter_map(|o| rr_remaining.get(&(set.key.phase_group.clone(), o.entrant_id.clone())))
            .copied()
            .max()
            .unwrap_or(1)
    }

    fn swiss_depth(&self, idx: usize, num_rounds: i32) -> u32 {
        let set = &self.sets[idx];
        if set.is_completed() {
            return 0;
        }
        (num_rounds - set.key.round + 1).max(1) as u32
    }

    /// Per (RR group, entrant): incomplete sets they still occupy there.
    fn rr_remaining_by_entrant(&self) -> HashMap<(String, EntrantId), u32> {
        let mut remaining = HashMap::new();
        for set in &self.sets {
            let is_rr = self
                .group(&set.key.phase_group)
                .is_some_and(|g| g.info.kind == GroupKind::RoundRobin);
            if !is_rr || set.is_completed() {
                continue;
            }
            for occupant in set.occupants() {
                *remaining
                    .entry((set.key.phase_group.clone(), occupant.entrant_id.clone()))
                    .or_insert(0) += 1;
            }
        }
        remaining
    }

    fn compute_unblocks(&mut self) {
        for idx in 0..self.sets.len() {
            self.unblock[idx] = self.dependents[idx]
                .iter()
                .filter(|d| !self.sets[d.set].is_completed() && !self.gf_reset_excluded[d.set])
                .count() as u32;
        }
    }

    /// Ironman material: incomplete occupied sets per player/entrant, plus
    /// one assumed set per future swiss round for each active swiss entrant.
    fn compute_remaining_counts(&mut self) {
        for (idx, set) in self.sets.iter().enumerate() {
            if set.is_completed() || self.gf_reset_excluded[idx] {
                continue;
            }
            for occupant in set.occupants() {
                if occupant.player_ids.is_empty() {
                    *self.remaining_by_entrant.entry(occupant.entrant_id.clone()).or_insert(0) += 1;
                } else {
                    for player in &occupant.player_ids {
                        *self.remaining_by_player.entry(player.clone()).or_insert(0) += 1;
                    }
                }
            }
        }

        for group in &self.groups {
            let GroupKind::Swiss { num_rounds } = group.info.kind else {
                continue;
            };
            let future_rounds = swiss_future_rounds(&self.sets, &group.set_indices, num_rounds);
            if future_rounds == 0 {
                continue;
            }
            for entrant in &group.active_entrants {
                let players: BTreeSet<_> = group
                    .set_indices
                    .iter()
                    .flat_map(|&i| self.sets[i].occupants())
                    .filter(|o| &o.entrant_id == entrant)
                    .flat_map(|o| o.player_ids.iter().cloned())
                    .collect();
                if players.is_empty() {
                    *self.remaining_by_entrant.entry(entrant.clone()).or_insert(0) += future_rounds;
                } else {
                    for player in players {
                        *self.remaining_by_player.entry(player).or_insert(0) += future_rounds;
                    }
                }
            }
        }
    }

    fn compute_group_rollups(&mut self) {
        let mut rollups = Vec::with_capacity(self.groups.len());
        for group in &self.groups {
            let incomplete: Vec<usize> = group
                .set_indices
                .iter()
                .copied()
                .filter(|&i| !self.sets[i].is_completed() && !self.gf_reset_excluded[i])
                .collect();
            let max_depth = incomplete.iter().map(|&i| self.depth[i]).max().unwrap_or(0);

            let (remaining_depth, swiss_future_demand) = match group.info.kind {
                GroupKind::Elimination | GroupKind::Unsupported(_) => (max_depth, 0),
                GroupKind::RoundRobin => {
                    let active = group
                        .set_indices
                        .iter()
                        .filter(|&&i| !self.sets[i].is_completed())
                        .flat_map(|&i| self.sets[i].occupants())
                        .map(|o| &o.entrant_id)
                        .collect::<BTreeSet<_>>()
                        .len() as u32;
                    let concurrency = (active / 2).max(1);
                    let serialized = (incomplete.len() as u32).div_ceil(concurrency);
                    (max_depth.max(serialized), 0)
                }
                GroupKind::Swiss { num_rounds } => {
                    let future = swiss_future_rounds(&self.sets, &group.set_indices, num_rounds);
                    let current_open = u32::from(!incomplete.is_empty());
                    let demand = future * (group.active_entrants.len() as u32 / 2);
                    (future + current_open, demand)
                }
            };
            rollups.push((remaining_depth, swiss_future_demand));
        }
        for (group, (depth, demand)) in self.groups.iter_mut().zip(rollups) {
            group.remaining_depth = depth;
            group.swiss_future_demand = demand;
        }

        self.remaining_critical_path = stage_runs(&self.groups)
            .into_iter()
            .map(|stage| stage.iter().map(|&i| self.groups[i].remaining_depth).max().unwrap_or(0))
            .sum();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Color {
    Unvisited,
    InProgress,
    Done,
}

/// Memoized DFS over downstream edges; `depth` doubles as the memo once a
/// node is `Done`. Free function so the recursion can hold split borrows of
/// the graph's fields.
fn elimination_depth(
    idx: usize,
    sets: &[LiveSet],
    dependents: &[Vec<Dependent>],
    excluded: &[bool],
    colors: &mut [Color],
    depth: &mut [u32],
    warnings: &mut BTreeSet<GraphWarning>,
) -> u32 {
    match colors[idx] {
        Color::Done => return depth[idx],
        Color::InProgress => {
            warnings.insert(GraphWarning::CycleDetected { at: sets[idx].key.clone() });
            return 0;
        }
        Color::Unvisited => {}
    }
    colors[idx] = Color::InProgress;

    let downstream = dependents[idx]
        .iter()
        .filter(|d| !excluded[d.set])
        .map(|d| elimination_depth(d.set, sets, dependents, excluded, colors, depth, warnings))
        .max()
        .unwrap_or(0);
    let result = u32::from(!sets[idx].is_completed()) + downstream;

    colors[idx] = Color::Done;
    depth[idx] = result;
    result
}

fn build_dependents(sets: &[LiveSet], by_id: &HashMap<SetId, usize>, warnings: &mut BTreeSet<GraphWarning>) -> Vec<Vec<Dependent>> {
    let mut dependents = vec![Vec::new(); sets.len()];
    for (idx, set) in sets.iter().enumerate() {
        for slot in &set.slots {
            let Some(Prereq::Set { id, placement }) = &slot.prereq else {
                continue;
            };
            match by_id.get(id) {
                Some(&feeder) => dependents[feeder].push(Dependent {
                    set: idx,
                    placement: *placement,
                }),
                // Permanent live behavior, not just pre-start: treat the slot
                // as pre-satisfied and remember (once) what was missing.
                None => {
                    warnings.insert(GraphWarning::DanglingPrereq { referenced: id.clone() });
                }
            }
        }
    }
    dependents
}

/// The GF-reset rule: both slots prereq the same set. Excluded until the
/// reset is real — its feeder decided *and* its slots actually filled.
fn gf_reset_exclusions(sets: &[LiveSet], by_id: &HashMap<SetId, usize>) -> Vec<bool> {
    sets.iter()
        .map(|set| {
            let feeder_ids: Vec<_> = set
                .slots
                .iter()
                .filter_map(|slot| match &slot.prereq {
                    Some(Prereq::Set { id, .. }) => Some(id),
                    _ => None,
                })
                .collect();
            let [a, b] = feeder_ids.as_slice() else {
                return false;
            };
            if a != b || set.is_completed() {
                return false;
            }
            let feeder_undecided = by_id.get(*a).is_none_or(|&i| !sets[i].is_completed());
            feeder_undecided || !set.all_slots_occupied()
        })
        .collect()
}

fn group_stats_skeleton(sets: &[LiveSet], infos: &[PhaseGroupInfo]) -> (Vec<GroupStats>, HashMap<String, usize>) {
    let group_index: HashMap<String, usize> = infos.iter().enumerate().map(|(i, g)| (g.id.clone(), i)).collect();

    let mut set_indices: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (idx, set) in sets.iter().enumerate() {
        if let Some(&g) = group_index.get(&set.key.phase_group) {
            set_indices.entry(g).or_default().push(idx);
        }
    }

    let groups = infos
        .iter()
        .enumerate()
        .map(|(i, info)| {
            let indices = set_indices.remove(&i).unwrap_or_default();
            let active_entrants: BTreeSet<_> = indices
                .iter()
                .flat_map(|&idx| sets[idx].occupants())
                .map(|o| o.entrant_id.clone())
                .collect();
            GroupStats {
                info: info.clone(),
                set_indices: indices,
                active_entrants: active_entrants.into_iter().collect(),
                remaining_depth: 0,
                swiss_future_demand: 0,
            }
        })
        .collect();
    (groups, group_index)
}

/// Rounds beyond the latest materialized one (live swiss only returns the
/// current round's sets).
fn swiss_future_rounds(sets: &[LiveSet], set_indices: &[usize], num_rounds: i32) -> u32 {
    let current_round = set_indices.iter().map(|&i| sets[i].key.round).max().unwrap_or(0);
    (num_rounds - current_round).max(0) as u32
}

/// Consecutive same-kind groups form one parallel stage (RR pools run side by
/// side); different-kind neighbors are sequential phases (pools → DE,
/// swiss → cut). Sequential same-kind phases are indistinguishable without
/// phase ids in the structure query — accepted S2 approximation.
fn stage_runs(groups: &[GroupStats]) -> Vec<Vec<usize>> {
    let mut stages: Vec<Vec<usize>> = Vec::new();
    let mut prev_kind: Option<&GroupKind> = None;
    for (i, group) in groups.iter().enumerate() {
        let same = prev_kind.is_some_and(|k| std::mem::discriminant(k) == std::mem::discriminant(&group.info.kind));
        if same {
            stages.last_mut().expect("same-kind run implies a prior stage").push(i);
        } else {
            stages.push(vec![i]);
        }
        prev_kind = Some(&group.info.kind);
    }
    stages
}

#[cfg(test)]
mod tests {
    use std::slice::from_ref;

    use super::{BracketGraph, GraphWarning};
    use crate::{
        model::{GroupKind, PlayerId, Prereq, SetId},
        synth::{complete, make_de_bracket, make_rr_pool, make_swiss, make_unseeded_se, materialize_ids},
    };

    const NOW: i64 = 1_751_000_000;

    #[test]
    fn depth_tripwire_57_entrant_de_r1_chain() {
        let bracket = make_de_bracket(9, 57);
        let (graph, _) = BracketGraph::build(&bracket.sets, &[bracket.info]);

        // The losers route from R1 is R1 + L1..L10 + GF = 12 incomplete sets.
        // This is the hideEmpty regression guard: losing the empty future
        // sets collapses this to ~1.
        let max_r1_depth = graph
            .sets()
            .iter()
            .enumerate()
            .filter(|(_, s)| s.key.round == 1)
            .map(|(i, _)| graph.depth(i))
            .max()
            .unwrap();
        assert_eq!(max_r1_depth, 12);
        assert!(max_r1_depth >= 10);
        assert_eq!(graph.remaining_critical_path(), 12);
    }

    #[test]
    fn de_57_danglings_are_deduplicated_warnings() {
        let bracket = make_de_bracket(9, 57);
        let (_, warnings) = BracketGraph::build(&bracket.sets, &[bracket.info]);
        let danglings = warnings.iter().filter(|w| matches!(w, GraphWarning::DanglingPrereq { .. })).count();
        assert_eq!(danglings, 7);
        assert_eq!(warnings.len(), 7);
    }

    #[test]
    fn same_missing_id_warns_once() {
        let mut bracket = make_de_bracket(9, 4);
        // Point both GF-reset slots at a set that doesn't exist.
        let missing = SetId("preview_9_99_0".to_owned());
        let reset = bracket.sets.iter_mut().find(|s| s.key.round == 4).unwrap();
        for slot in &mut reset.slots {
            if let Some(Prereq::Set { id, .. }) = &mut slot.prereq {
                *id = missing.clone();
            }
        }
        let (_, warnings) = BracketGraph::build(&bracket.sets, &[bracket.info]);
        assert_eq!(warnings, vec![GraphWarning::DanglingPrereq { referenced: missing }]);
    }

    #[test]
    fn completed_sets_pass_chains_through() {
        let mut bracket = make_de_bracket(9, 4);
        let w1_1_id = SetId("preview_9_1_1".to_owned());

        // Fresh bracket: the deepest route is W1[1] -> W2 (loser drops) ->
        // L2 -> GF = 4 incomplete sets.
        let (graph, _) = BracketGraph::build(&bracket.sets, from_ref(&bracket.info));
        let w1_1 = graph.index_of_id(&w1_1_id).unwrap();
        assert_eq!(graph.depth(w1_1), 4);

        // Complete W2 and L1 (timestamps alone; depth is structural). Every
        // route to L2 now crosses a completed set, so depth 3 (W1[1], L2,
        // GF) is only reachable if chains pass through completed sets.
        for round in [2, -1] {
            let idx = bracket.sets.iter().position(|s| s.key.round == round).unwrap();
            bracket.sets[idx].completed_at = Some(NOW);
        }
        let (graph, _) = BracketGraph::build(&bracket.sets, &[bracket.info]);
        let w1_1 = graph.index_of_id(&w1_1_id).unwrap();
        assert_eq!(graph.depth(w1_1), 3);
    }

    #[test]
    fn gf_reset_excluded_until_reachable() {
        let bracket = make_de_bracket(9, 4);
        let (graph, _) = BracketGraph::build(&bracket.sets, from_ref(&bracket.info));
        let gf = graph.index_of_id(&SetId("preview_9_3_0".to_owned())).unwrap();
        let reset = graph.index_of_id(&SetId("preview_9_4_0".to_owned())).unwrap();

        assert!(graph.is_gf_reset_excluded(reset));
        assert_eq!(graph.depth(reset), 0);
        assert_eq!(graph.depth(gf), 1, "GF's chain must not count the reset");
        assert_eq!(graph.unblock_count(gf), 0);
    }

    #[test]
    fn gf_reset_pruned_on_winners_side_completion_included_when_real() {
        let mut bracket = make_de_bracket(9, 4);
        let gf_pos = bracket.sets.iter().position(|s| s.key.round == 3).unwrap();
        let reset_pos = bracket.sets.iter().position(|s| s.key.round == 4).unwrap();
        let reset_id = SetId("preview_9_4_0".to_owned());

        // Winners-side champion: GF completed, reset never fills — pruned.
        bracket.sets[gf_pos].completed_at = Some(NOW);
        let (graph, _) = BracketGraph::build(&bracket.sets, from_ref(&bracket.info));
        let reset = graph.index_of_id(&reset_id).unwrap();
        assert!(graph.is_gf_reset_excluded(reset), "unfilled reset stays pruned");

        // Losers-side win: the server fills the reset's slots — now real.
        let finalists = &make_rr_pool(9, 2).sets[0].slots;
        for (slot, filled) in bracket.sets[reset_pos].slots.iter_mut().zip(finalists) {
            slot.occupant = filled.occupant.clone();
        }
        let (graph, _) = BracketGraph::build(&bracket.sets, &[bracket.info]);
        let reset = graph.index_of_id(&reset_id).unwrap();
        assert!(!graph.is_gf_reset_excluded(reset));
        assert_eq!(graph.depth(reset), 1);
    }

    #[test]
    fn rr_depth_follows_the_busiest_player_and_pool_concurrency() {
        // 3-player pool: 3 sets, but only one can run at a time.
        let pool = make_rr_pool(9, 3);
        let (graph, _) = BracketGraph::build(&pool.sets, &[pool.info]);
        assert_eq!(graph.depth(0), 2, "each player has 2 sets left");
        assert_eq!(graph.remaining_critical_path(), 3, "pool-aware term: 3 sets / 1 concurrent");

        // 4-player pool: per-player depth 3 dominates ceil(6/2)=3.
        let pool = make_rr_pool(9, 4);
        let mut sets = pool.sets;
        complete(&mut sets[0], 0, NOW);
        let (graph, _) = BracketGraph::build(&sets, &[pool.info]);
        let untouched = graph
            .sets()
            .iter()
            .position(|s| {
                !s.is_completed()
                    && s.occupants()
                        .all(|o| !graph.sets()[0].occupants().any(|c| c.entrant_id == o.entrant_id))
            })
            .unwrap();
        assert_eq!(graph.depth(untouched), 3);
        assert_eq!(graph.remaining_critical_path(), 3, "max(3, ceil(5/2))");
    }

    #[test]
    fn swiss_depth_is_remaining_rounds() {
        let swiss = make_swiss(9, 9, 4);
        let (graph, _) = BracketGraph::build(&swiss.sets, from_ref(&swiss.info));
        assert!(graph.sets().iter().all(|s| s.key.round == 1));
        assert_eq!(graph.depth(0), 4);
        assert_eq!(graph.remaining_critical_path(), 4, "3 future rounds + current");
        assert_eq!(graph.group("9").unwrap().swiss_future_demand, 3 * 4);

        let mut sets = swiss.sets;
        for set in &mut sets {
            complete(set, 0, NOW);
        }
        let (graph, _) = BracketGraph::build(&sets, &[swiss.info]);
        assert_eq!(graph.remaining_critical_path(), 3, "3 future rounds, current closed");
    }

    #[test]
    fn mixed_event_stages_sum() {
        // rust_vitational shape: two RR pools (parallel) then an elimination cut.
        let pool_a = make_rr_pool(11, 4);
        let pool_b = make_rr_pool(12, 4);
        let cut = make_unseeded_se(13, 4);
        let sets: Vec<_> = pool_a.sets.into_iter().chain(pool_b.sets).chain(cut.sets).collect();
        let infos = vec![pool_a.info, pool_b.info, cut.info];

        let (graph, warnings) = BracketGraph::build(&sets, &infos);
        assert!(warnings.is_empty());
        assert_eq!(graph.group("11").unwrap().remaining_depth, 3);
        assert_eq!(graph.group("13").unwrap().remaining_depth, 2, "SE(4): semi + final");
        assert_eq!(graph.remaining_critical_path(), 3 + 2, "pools stage + cut stage");
        assert_eq!(graph.groups()[0].info.kind, GroupKind::RoundRobin);
    }

    #[test]
    fn completed_event_has_zero_remaining() {
        let bracket = make_de_bracket(9, 4);
        let mut sets = bracket.sets;
        // GF-reset never fired: complete everything but it.
        for set in &mut sets {
            if set.key.round != 4 {
                set.completed_at = Some(NOW);
            }
        }
        let (graph, _) = BracketGraph::build(&sets, &[bracket.info]);
        assert_eq!(graph.remaining_critical_path(), 0);
        assert_eq!(graph.remaining_for_player(&PlayerId("P1".to_owned())), 0);
    }

    #[test]
    fn preview_and_numeric_forms_agree() {
        let bracket = make_de_bracket(9, 8);
        let numeric_sets = materialize_ids(&bracket.sets, 5000);

        let (preview_graph, w1) = BracketGraph::build(&bracket.sets, from_ref(&bracket.info));
        let (numeric_graph, w2) = BracketGraph::build(&numeric_sets, &[bracket.info]);

        assert_eq!(w1, w2);
        for (i, set) in preview_graph.sets().iter().enumerate() {
            let j = numeric_graph.index_of_key(&set.key).expect("keys survive the swap");
            assert_eq!(preview_graph.depth(i), numeric_graph.depth(j));
            assert_eq!(preview_graph.unblock_count(i), numeric_graph.unblock_count(j));
        }
    }

    #[test]
    fn remaining_counts_feed_ironman() {
        let bracket = make_de_bracket(9, 4);
        let (graph, _) = BracketGraph::build(&bracket.sets, &[bracket.info]);
        // Seed 1 occupies exactly one incomplete set (their R1).
        assert_eq!(graph.remaining_for_player(&PlayerId("P1".to_owned())), 1);

        let swiss = make_swiss(10, 8, 4);
        let (graph, _) = BracketGraph::build(&swiss.sets, &[swiss.info]);
        // Current round set + 3 assumed future rounds.
        assert_eq!(graph.remaining_for_player(&PlayerId("P1".to_owned())), 4);
    }

    #[test]
    fn unknown_phase_group_warns_and_defaults_to_elimination() {
        let bracket = make_de_bracket(9, 4);
        let (graph, warnings) = BracketGraph::build(&bracket.sets, &[]);
        assert!(warnings.contains(&GraphWarning::UnknownPhaseGroup {
            phase_group: "9".to_owned()
        }));
        // DAG depth still works without structure info.
        let gf = graph.index_of_id(&SetId("preview_9_3_0".to_owned())).unwrap();
        assert_eq!(graph.depth(gf), 1);
    }
}

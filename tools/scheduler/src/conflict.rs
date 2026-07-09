//! Cross-bracket conflict tracking and the callable predicate.
//!
//! Conflict keys are global player ids, alias-merged; an occupant whose
//! identity degraded (no player ids) falls back to its entrant id, which only
//! serializes within its own event (the same human holds a different entrant
//! id elsewhere). All local wall-clock inputs are unix millis; server
//! timestamps (unix seconds) are converted at the comparison boundary.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::{
    config::{BracketMode, SetupId, DEFAULT_SETUP_TYPE},
    graph::BracketGraph,
    model::{BracketId, EntrantId, LiveSet, PlayerId, SetId, SetKey, SlotOccupant},
};

pub type UnixMillis = i64;

/// Who a busy/blocked check is about: a canonical (alias-merged) player, or
/// the entrant-scoped fallback for identity-degraded occupants.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ConflictKey {
    Player(PlayerId),
    Entrant(EntrantId),
}

/// `player id → canonical representative` built from config alias sets plus
/// any session overlay links (pass them concatenated). The representative is
/// the numerically smallest id so merges are order-independent.
#[derive(Debug, Clone, Default)]
pub struct AliasMap {
    canonical: HashMap<PlayerId, PlayerId>,
}

impl AliasMap {
    pub fn build(alias_sets: &[Vec<PlayerId>]) -> Self {
        let mut parent: HashMap<PlayerId, PlayerId> = HashMap::new();
        for set in alias_sets {
            for pair in set.windows(2) {
                union(&mut parent, &pair[0], &pair[1]);
            }
        }

        // Path-compress into a flat map, picking the smallest member of each
        // group as its representative.
        let members: Vec<PlayerId> = parent.keys().cloned().collect();
        let mut groups: HashMap<PlayerId, Vec<PlayerId>> = HashMap::new();
        for player in members {
            let root = find(&parent, &player);
            groups.entry(root).or_default().push(player);
        }
        let canonical = groups
            .into_values()
            .flat_map(|group| {
                let representative = group
                    .iter()
                    .min_by_key(|p| numeric_order(p))
                    .cloned()
                    .expect("groups are non-empty");
                group.into_iter().map(move |p| (p, representative.clone()))
            })
            .collect();
        Self { canonical }
    }

    pub fn canonical(&self, player: &PlayerId) -> PlayerId {
        self.canonical.get(player).cloned().unwrap_or_else(|| player.clone())
    }
}

fn find(parent: &HashMap<PlayerId, PlayerId>, player: &PlayerId) -> PlayerId {
    let mut current = player.clone();
    while let Some(next) = parent.get(&current) {
        if *next == current {
            break;
        }
        current = next.clone();
    }
    current
}

fn union(parent: &mut HashMap<PlayerId, PlayerId>, a: &PlayerId, b: &PlayerId) {
    parent.entry(a.clone()).or_insert_with(|| a.clone());
    parent.entry(b.clone()).or_insert_with(|| b.clone());
    let (root_a, root_b) = (find(parent, a), find(parent, b));
    if root_a != root_b {
        parent.insert(root_a, root_b);
    }
}

/// Digit-string ordering: shorter ids are smaller; ties break lexically.
fn numeric_order(player: &PlayerId) -> (usize, &str) {
    (player.0.len(), player.0.as_str())
}

/// The conflict keys an occupant contributes (one per participant player id,
/// or the entrant fallback when identity degraded).
pub fn occupant_keys(occupant: &SlotOccupant, aliases: &AliasMap) -> Vec<ConflictKey> {
    if occupant.player_ids.is_empty() {
        vec![ConflictKey::Entrant(occupant.entrant_id.clone())]
    } else {
        occupant
            .player_ids
            .iter()
            .map(|p| ConflictKey::Player(aliases.canonical(p)))
            .collect()
    }
}

/// What a station is doing right now. `Called`/`InProgress` are our own
/// (local-overlay) actions; `OccupiedExternal` is someone else using the
/// setup, optionally tied to a tracked set that will free it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SetupStatus {
    Free,
    Called { bracket: BracketId, set: SetKey },
    InProgress { bracket: BracketId, set: SetKey },
    OccupiedExternal { set: Option<(BracketId, SetKey)> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Setup {
    pub id: SetupId,
    pub status: SetupStatus,
    /// The hardware class this station belongs to; brackets pool by type.
    /// Empty only in pre-migration persisted overlays (detected on load).
    #[serde(default)]
    pub setup_type: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SetupBoard {
    setups: Vec<Setup>,
}

impl SetupBoard {
    pub fn new(ids: &[SetupId]) -> Self {
        Self::from_roster(&ids.iter().map(|&id| (id, DEFAULT_SETUP_TYPE.to_owned())).collect::<Vec<_>>())
    }

    pub fn from_roster(roster: &[(SetupId, String)]) -> Self {
        Self {
            setups: roster
                .iter()
                .map(|(id, setup_type)| Setup {
                    id: *id,
                    status: SetupStatus::Free,
                    setup_type: setup_type.clone(),
                })
                .collect(),
        }
    }

    pub fn set_status(&mut self, id: SetupId, status: SetupStatus) {
        if let Some(setup) = self.setups.iter_mut().find(|s| s.id == id) {
            setup.status = status;
        }
    }

    /// Adds a free station, keeping the board in id order (the strip renders
    /// board order; placards are numeric).
    pub fn add_setup(&mut self, id: SetupId, setup_type: String) {
        let setup = Setup {
            id,
            status: SetupStatus::Free,
            setup_type,
        };
        let at = self.setups.partition_point(|s| s.id < id);
        self.setups.insert(at, setup);
    }

    pub fn remove_setup(&mut self, id: SetupId) {
        self.setups.retain(|s| s.id != id);
    }

    /// The smallest positive station number not on the board — a retired
    /// number is reused (physical placard reuse).
    pub fn lowest_unused_id(&self) -> SetupId {
        let taken: HashSet<u32> = self.setups.iter().map(|s| s.id.0).collect();
        SetupId((1..).find(|n| !taken.contains(n)).expect("u32 range never exhausts"))
    }

    pub fn counts_by_type(&self) -> BTreeMap<String, u32> {
        let mut counts = BTreeMap::new();
        for setup in &self.setups {
            *counts.entry(setup.setup_type.clone()).or_insert(0) += 1;
        }
        counts
    }

    pub fn setups(&self) -> &[Setup] {
        &self.setups
    }

    pub fn free_ids(&self) -> impl Iterator<Item = SetupId> + '_ {
        self.setups.iter().filter(|s| s.status == SetupStatus::Free).map(|s| s.id)
    }
}

/// A per-setup pool override (the `a` reassignment): where a setup may take
/// calls from, superseding every bracket's config pool for that setup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PoolOverride {
    /// Only this bracket may call on the setup.
    Dedicated(BracketId),
    /// Any full bracket may call on the setup.
    AllowAny,
}

/// One bracket's effective pool: the roster's matching-type stations that
/// aren't overridden away, plus stations dedicated to this bracket or opened
/// to all.
pub fn effective_pool(
    bracket: &BracketId,
    setup_types: &[String],
    roster: &[Setup],
    overrides: &HashMap<SetupId, PoolOverride>,
) -> Vec<SetupId> {
    let mut pool: Vec<SetupId> = roster
        .iter()
        .filter(|s| setup_types.contains(&s.setup_type) && !overrides.contains_key(&s.id))
        .map(|s| s.id)
        .collect();
    for setup in roster {
        let extra = match overrides.get(&setup.id) {
            Some(PoolOverride::Dedicated(b)) => b == bracket,
            Some(PoolOverride::AllowAny) => true,
            None => false,
        };
        if extra && !pool.contains(&setup.id) {
            pool.push(setup.id);
        }
    }
    pool.sort();
    pool
}

/// TO-set player state, keyed by canonical conflict key.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlayerFlags {
    pub resting: HashSet<ConflictKey>,
    pub departed: HashSet<ConflictKey>,
    /// TO override: ignore remote busy-evidence for this player ("they are
    /// standing right here"). Local overlay evidence is never suppressed.
    pub force_available: HashSet<ConflictKey>,
}

/// Per-set local knowledge that outlives a poll, keyed by (bracket, SetKey)
/// so it survives the preview→numeric id swap.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Tombstones {
    /// We reported/observed a finish locally; the server hasn't confirmed.
    pub awaiting_remote_completion: HashSet<(BracketId, SetKey)>,
    /// TO override: ignore this set's remote startedAt evidence.
    pub suppress_remote_active: HashSet<(BracketId, SetKey)>,
    /// TO override: ignore this set's CALLED state-int evidence.
    pub suppress_remote_called: HashSet<(BracketId, SetKey)>,
}

/// One bracket's slice of the world, as the callable predicate needs it.
#[derive(Debug, Clone)]
pub struct BracketView<'a> {
    pub id: &'a BracketId,
    pub sets: &'a [LiveSet],
    pub mode: BracketMode,
    /// Unix seconds (server domain), config override already applied.
    pub start_at: Option<i64>,
    pub held: bool,
    /// Effective setup pool (config pool in S2; S4 adds overrides).
    pub pool: &'a [SetupId],
}

/// Everything the busy index and callable predicate consume besides the
/// bracket views themselves. Tests populate these directly; S3's TUI owns
/// them live.
#[derive(Debug, Clone)]
pub struct ConflictInputs<'a> {
    pub aliases: &'a AliasMap,
    pub board: &'a SetupBoard,
    pub flags: &'a PlayerFlags,
    pub tombstones: &'a Tombstones,
    /// Learned/config-pinned state ints meaning CALLED. Only these are
    /// busy-evidence; unknown ints never are.
    pub called_ints: &'a [i32],
    /// Sets escalated to soft-busy by an unpinned state-int deviation
    /// (config-gated; the caller decides what's in here).
    pub soft_busy: &'a [(BracketId, SetKey)],
    /// Per-key completion times (unix millis) feeding rest windows.
    pub last_completed: &'a HashMap<ConflictKey, UnixMillis>,
    pub rest_window_secs: u64,
    /// Set-level "don't suggest until" (unix millis).
    pub snoozes: &'a HashMap<(BracketId, SetKey), UnixMillis>,
}

/// Why a conflict key counts as busy, in descending evidence precedence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BusySource {
    /// Our own call/start on a station (never suppressed).
    LocalSetup { setup: SetupId, bracket: BracketId, set: SetKey },
    /// Remote `startedAt` on an uncompleted set.
    RemoteActive { bracket: BracketId, set: SetKey },
    /// A learned/pinned CALLED state int.
    RemoteCalled { bracket: BracketId, set: SetKey },
    /// Escalated unpinned state-int deviation.
    SoftDeviation { bracket: BracketId, set: SetKey },
}

impl BusySource {
    fn precedence(&self) -> u8 {
        match self {
            Self::LocalSetup { .. } => 3,
            Self::RemoteActive { .. } => 2,
            Self::RemoteCalled { .. } => 1,
            Self::SoftDeviation { .. } => 0,
        }
    }

    fn names_set(&self, bracket: &BracketId, key: &SetKey) -> bool {
        let (Self::LocalSetup { bracket: b, set, .. }
        | Self::RemoteActive { bracket: b, set }
        | Self::RemoteCalled { bracket: b, set }
        | Self::SoftDeviation { bracket: b, set }) = self;
        b == bracket && set == key
    }
}

/// The per-snapshot busy map over canonical conflict keys.
#[derive(Debug, Clone, Default)]
pub struct ConflictIndex {
    busy: HashMap<ConflictKey, BusySource>,
    resting_until: HashMap<ConflictKey, UnixMillis>,
}

impl ConflictIndex {
    pub fn build(views: &[BracketView<'_>], inputs: &ConflictInputs<'_>) -> Self {
        let mut index = Self::default();
        let set_lookup: HashMap<(&BracketId, &SetKey), &LiveSet> =
            views.iter().flat_map(|v| v.sets.iter().map(move |s| ((v.id, &s.key), s))).collect();

        // Local overlay: whatever the setup board says is running/called.
        for setup in inputs.board.setups() {
            let (bracket, key) = match &setup.status {
                SetupStatus::Called { bracket, set } | SetupStatus::InProgress { bracket, set } => (bracket, set),
                SetupStatus::OccupiedExternal { set: Some((bracket, set)) } => (bracket, set),
                SetupStatus::Free | SetupStatus::OccupiedExternal { set: None } => continue,
            };
            let Some(set) = set_lookup.get(&(bracket, key)) else {
                continue;
            };
            let source = BusySource::LocalSetup {
                setup: setup.id,
                bracket: bracket.clone(),
                set: key.clone(),
            };
            for occupant in set.occupants() {
                for conflict_key in occupant_keys(occupant, inputs.aliases) {
                    index.claim(conflict_key, source.clone());
                }
            }
        }

        // Remote evidence, force-available-suppressed per key.
        for view in views {
            for set in view.sets {
                if set.is_completed() {
                    continue;
                }
                let at = (view.id.clone(), set.key.clone());
                if set.is_remotely_active() && !inputs.tombstones.suppress_remote_active.contains(&at) {
                    index.claim_remote(set, view.id, inputs, |bracket, key| BusySource::RemoteActive { bracket, set: key });
                }
                if set.called_evidence(inputs.called_ints) && !inputs.tombstones.suppress_remote_called.contains(&at) {
                    index.claim_remote(set, view.id, inputs, |bracket, key| BusySource::RemoteCalled { bracket, set: key });
                }
                if inputs.soft_busy.contains(&at) {
                    index.claim_remote(set, view.id, inputs, |bracket, key| BusySource::SoftDeviation { bracket, set: key });
                }
            }
        }

        if inputs.rest_window_secs > 0 {
            let window = inputs.rest_window_secs as i64 * 1000;
            index.resting_until = inputs.last_completed.iter().map(|(k, &t)| (k.clone(), t + window)).collect();
        }
        index
    }

    fn claim(&mut self, key: ConflictKey, source: BusySource) {
        match self.busy.get(&key) {
            Some(existing) if existing.precedence() >= source.precedence() => {}
            _ => {
                self.busy.insert(key, source);
            }
        }
    }

    fn claim_remote(
        &mut self,
        set: &LiveSet,
        bracket: &BracketId,
        inputs: &ConflictInputs<'_>,
        make: impl Fn(BracketId, SetKey) -> BusySource,
    ) {
        for occupant in set.occupants() {
            for key in occupant_keys(occupant, inputs.aliases) {
                if inputs.flags.force_available.contains(&key) {
                    continue;
                }
                self.claim(key, make(bracket.clone(), set.key.clone()));
            }
        }
    }

    pub fn busy_source(&self, key: &ConflictKey) -> Option<&BusySource> {
        self.busy.get(key)
    }

    pub fn rest_expiry(&self, key: &ConflictKey) -> Option<UnixMillis> {
        self.resting_until.get(key).copied()
    }

    /// Extends (never shortens) a key's rest expiry. The simulator uses this
    /// to model resting players returning after the sim horizon.
    pub fn extend_rest(&mut self, key: ConflictKey, until: UnixMillis) {
        let entry = self.resting_until.entry(key).or_insert(until);
        *entry = (*entry).max(until);
    }
}

/// A set cleared to call, with the stations it could go on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallableSet {
    pub bracket: BracketId,
    pub key: SetKey,
    pub id: SetId,
    pub candidate_setups: Vec<SetupId>,
}

/// Every reason a set is not callable right now — all retained, never just
/// the first, so the UI can explain "why not this one?".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockReason {
    ConflictOnlyBracket,
    Completed,
    /// Remote startedAt says it's already running (tombstone-suppressible).
    RemotelyActive,
    /// A learned CALLED int says someone already called it.
    RemotelyCalled,
    AwaitingRemoteCompletion,
    SlotsUnresolved,
    HasPlaceholder,
    BracketHeld,
    BracketNotOpen {
        starts_at: Option<i64>,
    },
    NoPermittedFreeSetup,
    PlayerBusy {
        key: ConflictKey,
        source: BusySource,
    },
    PlayerResting {
        key: ConflictKey,
    },
    PlayerDeparted {
        key: ConflictKey,
    },
    RestWindow {
        key: ConflictKey,
        until: UnixMillis,
    },
    PlayerDisqualified {
        key: ConflictKey,
    },
    Snoozed {
        until: UnixMillis,
    },
}

/// The callable predicate. Collects *all* block reasons for the set.
pub fn callable(
    view: &BracketView<'_>,
    set: &LiveSet,
    index: &ConflictIndex,
    inputs: &ConflictInputs<'_>,
    now_millis: UnixMillis,
) -> Result<CallableSet, Vec<BlockReason>> {
    let mut reasons = Vec::new();

    if view.mode == BracketMode::ConflictOnly {
        reasons.push(BlockReason::ConflictOnlyBracket);
    }
    if set.is_completed() {
        reasons.push(BlockReason::Completed);
    }

    let at = (view.id.clone(), set.key.clone());
    if set.is_remotely_active() && !inputs.tombstones.suppress_remote_active.contains(&at) {
        reasons.push(BlockReason::RemotelyActive);
    }
    if set.called_evidence(inputs.called_ints) && !inputs.tombstones.suppress_remote_called.contains(&at) {
        reasons.push(BlockReason::RemotelyCalled);
    }
    if inputs.tombstones.awaiting_remote_completion.contains(&at) {
        reasons.push(BlockReason::AwaitingRemoteCompletion);
    }

    if !set.all_slots_occupied() {
        reasons.push(BlockReason::SlotsUnresolved);
    }
    if set.has_placeholder {
        reasons.push(BlockReason::HasPlaceholder);
    }

    if view.held {
        reasons.push(BlockReason::BracketHeld);
    }
    if view.start_at.is_some_and(|s| s * 1000 > now_millis) {
        reasons.push(BlockReason::BracketNotOpen { starts_at: view.start_at });
    }

    let candidate_setups: Vec<SetupId> = inputs.board.free_ids().filter(|id| view.pool.contains(id)).collect();
    if candidate_setups.is_empty() {
        reasons.push(BlockReason::NoPermittedFreeSetup);
    }

    for occupant in set.occupants() {
        for key in occupant_keys(occupant, inputs.aliases) {
            if occupant.is_disqualified {
                reasons.push(BlockReason::PlayerDisqualified { key: key.clone() });
            }
            if inputs.flags.departed.contains(&key) {
                reasons.push(BlockReason::PlayerDeparted { key: key.clone() });
            }
            if inputs.flags.resting.contains(&key) {
                reasons.push(BlockReason::PlayerResting { key: key.clone() });
            }
            // A set never blocks itself: skip busy evidence it generated.
            if let Some(source) = index.busy_source(&key) {
                if !source.names_set(view.id, &set.key) {
                    reasons.push(BlockReason::PlayerBusy {
                        key: key.clone(),
                        source: source.clone(),
                    });
                }
            }
            if let Some(until) = index.rest_expiry(&key) {
                if until > now_millis {
                    reasons.push(BlockReason::RestWindow { key, until });
                }
            }
        }
    }

    if let Some(&until) = inputs.snoozes.get(&at) {
        if until > now_millis {
            reasons.push(BlockReason::Snoozed { until });
        }
    }

    if reasons.is_empty() {
        Ok(CallableSet {
            bracket: view.id.clone(),
            key: set.key.clone(),
            id: set.id.clone(),
            candidate_setups,
        })
    } else {
        Err(reasons)
    }
}

/// All callable sets across the world, in view/set order.
pub fn callable_sets(
    views: &[BracketView<'_>],
    index: &ConflictIndex,
    inputs: &ConflictInputs<'_>,
    now_millis: UnixMillis,
) -> Vec<CallableSet> {
    views
        .iter()
        .flat_map(|view| {
            view.sets
                .iter()
                .filter_map(|set| callable(view, set, index, inputs, now_millis).ok())
        })
        .collect()
}

/// The world index for the ironman term: remaining set counts per canonical
/// conflict key, aggregated across every bracket's graph.
pub fn aggregate_remaining(graphs: &[(&BracketId, &BracketGraph)], aliases: &AliasMap) -> HashMap<ConflictKey, u32> {
    let mut totals: HashMap<ConflictKey, u32> = HashMap::new();
    for (_, graph) in graphs {
        for (player, count) in graph.remaining_player_counts() {
            *totals.entry(ConflictKey::Player(aliases.canonical(player))).or_insert(0) += count;
        }
        for (entrant, count) in graph.remaining_entrant_counts() {
            *totals.entry(ConflictKey::Entrant(entrant.clone())).or_insert(0) += count;
        }
    }
    totals
}

/// A state int changed to a value we can't interpret. Advisory by default;
/// config may escalate it to soft-busy (the caller then adds the set to
/// [`ConflictInputs::soft_busy`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateDeviation {
    pub key: SetKey,
    pub from: Option<i32>,
    pub to: Option<i32>,
}

pub fn state_deviation(set: &LiveSet, baseline: &LiveSet, known_ints: &[i32]) -> Option<StateDeviation> {
    if set.state_int == baseline.state_int {
        return None;
    }
    if set.state_int.is_some_and(|s| known_ints.contains(&s)) {
        return None;
    }
    Some(StateDeviation {
        key: set.key.clone(),
        from: baseline.state_int,
        to: set.state_int,
    })
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, slice::from_ref};

    use super::{
        aggregate_remaining, callable, callable_sets, effective_pool, occupant_keys, state_deviation, AliasMap, BlockReason, BracketView,
        BusySource, CallableSet, ConflictIndex, ConflictInputs, ConflictKey, PlayerFlags, PoolOverride, Setup, SetupBoard, SetupStatus,
        Tombstones,
    };
    use crate::{
        config::{BracketMode, SetupId},
        graph::BracketGraph,
        model::{BracketId, EntrantId, LiveSet, PlayerId, Prereq, SetId, SetKey, Slot, SlotOccupant, PREREQ_TYPE_SEED},
    };

    const NOW: i64 = 1_751_000_000_000;

    fn player(id: &str) -> PlayerId {
        PlayerId(id.to_owned())
    }

    fn key_of(player_id: &str) -> ConflictKey {
        ConflictKey::Player(player(player_id))
    }

    fn occupant(entrant: &str, players: &[&str]) -> SlotOccupant {
        SlotOccupant {
            entrant_id: EntrantId(entrant.to_owned()),
            display_name: entrant.to_owned(),
            is_disqualified: false,
            player_ids: players.iter().map(|p| player(p)).collect(),
        }
    }

    fn set_between(pg: &str, identifier: &str, a: SlotOccupant, b: SlotOccupant) -> LiveSet {
        let slot = |o| Slot {
            prereq: Some(Prereq::PreSatisfied {
                raw_type: Some(PREREQ_TYPE_SEED.to_owned()),
            }),
            occupant: Some(o),
        };
        LiveSet {
            id: SetId(format!("{pg}-{identifier}")),
            key: SetKey {
                phase_group: pg.to_owned(),
                round: 1,
                identifier: identifier.to_owned(),
            },
            state_int: Some(1),
            full_round_text: None,
            started_at: None,
            completed_at: None,
            winner_id: None,
            has_placeholder: false,
            slots: vec![slot(a), slot(b)],
        }
    }

    fn simple_set(pg: &str, identifier: &str, players: [&str; 2]) -> LiveSet {
        set_between(
            pg,
            identifier,
            occupant(&format!("{pg}-e-{}", players[0]), &[players[0]]),
            occupant(&format!("{pg}-e-{}", players[1]), &[players[1]]),
        )
    }

    /// One-bracket world with a single free setup; tests mutate from here.
    struct World {
        bracket: BracketId,
        pool: Vec<SetupId>,
        board: SetupBoard,
        aliases: AliasMap,
        flags: PlayerFlags,
        tombstones: Tombstones,
        called_ints: Vec<i32>,
        soft_busy: Vec<(BracketId, SetKey)>,
        last_completed: HashMap<ConflictKey, i64>,
        rest_window_secs: u64,
        snoozes: HashMap<(BracketId, SetKey), i64>,
    }

    impl World {
        fn new() -> Self {
            Self {
                bracket: BracketId("melee".to_owned()),
                pool: vec![SetupId(1), SetupId(2)],
                board: SetupBoard::new(&[SetupId(1), SetupId(2)]),
                aliases: AliasMap::default(),
                flags: PlayerFlags::default(),
                tombstones: Tombstones::default(),
                called_ints: vec![6],
                soft_busy: Vec::new(),
                last_completed: HashMap::new(),
                rest_window_secs: 0,
                snoozes: HashMap::new(),
            }
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
                snoozes: &self.snoozes,
            }
        }

        fn view<'a>(&'a self, sets: &'a [LiveSet]) -> BracketView<'a> {
            BracketView {
                id: &self.bracket,
                sets,
                mode: BracketMode::Full,
                start_at: None,
                held: false,
                pool: &self.pool,
            }
        }

        fn check(&self, sets: &[LiveSet], target: usize) -> Result<CallableSet, Vec<BlockReason>> {
            let view = self.view(sets);
            let index = ConflictIndex::build(from_ref(&view), &self.inputs());
            callable(&view, &sets[target], &index, &self.inputs(), NOW)
        }
    }

    #[test]
    fn clean_set_is_callable_with_candidate_setups() {
        let world = World::new();
        let sets = [simple_set("77", "A", ["10", "11"])];
        let callable = world.check(&sets, 0).expect("nothing blocks it");
        assert_eq!(callable.candidate_setups, vec![SetupId(1), SetupId(2)]);
        assert_eq!(callable.key, sets[0].key);
    }

    #[test]
    fn each_condition_yields_its_block_reason() {
        let mut sets = [simple_set("77", "A", ["10", "11"])];

        let mut world = World::new();
        world.rest_window_secs = 300;
        world.last_completed.insert(key_of("10"), NOW - 60_000);
        world.flags.resting.insert(key_of("11"));
        world.flags.departed.insert(key_of("10"));
        world.snoozes.insert((world.bracket.clone(), sets[0].key.clone()), NOW + 60_000);
        world.board.set_status(SetupId(1), SetupStatus::OccupiedExternal { set: None });
        world.board.set_status(SetupId(2), SetupStatus::OccupiedExternal { set: None });
        sets[0].completed_at = Some(NOW / 1000 - 10);
        sets[0].has_placeholder = true;
        sets[0].slots[0].occupant.as_mut().unwrap().is_disqualified = true;

        let reasons = world.check(&sets, 0).unwrap_err();
        let expect = [
            BlockReason::Completed,
            BlockReason::HasPlaceholder,
            BlockReason::NoPermittedFreeSetup,
            BlockReason::PlayerDisqualified { key: key_of("10") },
            BlockReason::PlayerDeparted { key: key_of("10") },
            BlockReason::PlayerResting { key: key_of("11") },
            BlockReason::RestWindow {
                key: key_of("10"),
                until: NOW + 240_000,
            },
            BlockReason::Snoozed { until: NOW + 60_000 },
        ];
        for reason in &expect {
            assert!(reasons.contains(reason), "missing {reason:?} in {reasons:?}");
        }
        assert_eq!(reasons.len(), expect.len(), "no spurious extras: {reasons:?}");
    }

    #[test]
    fn future_sets_are_blocked_on_unresolved_slots() {
        let world = World::new();
        let bracket = crate::synth::make_de_bracket(77, 4);
        let w2 = bracket.sets.iter().position(|s| s.key.round == 2).unwrap();
        let reasons = world.check(&bracket.sets, w2).unwrap_err();
        assert_eq!(reasons, vec![BlockReason::SlotsUnresolved]);
    }

    #[test]
    fn bracket_gates_hold_and_start_time() {
        let world = World::new();
        let sets = [simple_set("77", "A", ["10", "11"])];

        let mut view = world.view(&sets);
        view.held = true;
        view.start_at = Some(NOW / 1000 + 3600);
        let index = ConflictIndex::build(from_ref(&view), &world.inputs());
        let reasons = callable(&view, &sets[0], &index, &world.inputs(), NOW).unwrap_err();
        assert!(reasons.contains(&BlockReason::BracketHeld));
        assert!(reasons.contains(&BlockReason::BracketNotOpen {
            starts_at: Some(NOW / 1000 + 3600)
        }));

        view.held = false;
        view.start_at = Some(NOW / 1000 - 60);
        let index = ConflictIndex::build(from_ref(&view), &world.inputs());
        assert!(callable(&view, &sets[0], &index, &world.inputs(), NOW).is_ok());
    }

    #[test]
    fn remote_evidence_blocks_and_tombstones_suppress() {
        let mut world = World::new();
        let mut sets = [simple_set("77", "A", ["10", "11"])];

        sets[0].started_at = Some(NOW / 1000 - 300);
        let reasons = world.check(&sets, 0).unwrap_err();
        assert_eq!(reasons, vec![BlockReason::RemotelyActive]);

        world
            .tombstones
            .suppress_remote_active
            .insert((world.bracket.clone(), sets[0].key.clone()));
        assert!(world.check(&sets, 0).is_ok(), "suppressed evidence unblocks");

        sets[0].started_at = None;
        sets[0].state_int = Some(6);
        let reasons = world.check(&sets, 0).unwrap_err();
        assert_eq!(reasons, vec![BlockReason::RemotelyCalled]);

        world.called_ints.clear();
        assert!(world.check(&sets, 0).is_ok(), "an unlearned int is not evidence");

        world
            .tombstones
            .awaiting_remote_completion
            .insert((world.bracket.clone(), sets[0].key.clone()));
        let reasons = world.check(&sets, 0).unwrap_err();
        assert_eq!(reasons, vec![BlockReason::AwaitingRemoteCompletion]);
    }

    #[test]
    fn busy_evidence_blocks_other_sets_not_itself() {
        let world = World::new();
        let mut sets = [simple_set("77", "A", ["10", "11"]), simple_set("77", "B", ["10", "12"])];
        sets[0].started_at = Some(NOW / 1000 - 300);

        // The active set blocks its sibling through the shared player...
        let reasons = world.check(&sets, 1).unwrap_err();
        assert_eq!(
            reasons,
            vec![BlockReason::PlayerBusy {
                key: key_of("10"),
                source: BusySource::RemoteActive {
                    bracket: world.bracket.clone(),
                    set: sets[0].key.clone(),
                },
            }]
        );

        // ...but the active set's own block list is only RemotelyActive, not
        // "busy because of itself".
        let reasons = world.check(&sets, 0).unwrap_err();
        assert_eq!(reasons, vec![BlockReason::RemotelyActive]);
    }

    #[test]
    fn local_overlay_outranks_remote_evidence() {
        let mut world = World::new();
        let mut sets = [simple_set("77", "A", ["10", "11"])];
        sets[0].started_at = Some(NOW / 1000 - 60);
        world.board.set_status(
            SetupId(1),
            SetupStatus::InProgress {
                bracket: world.bracket.clone(),
                set: sets[0].key.clone(),
            },
        );

        let view = world.view(&sets);
        let index = ConflictIndex::build(from_ref(&view), &world.inputs());
        assert!(matches!(
            index.busy_source(&key_of("10")),
            Some(BusySource::LocalSetup { setup: SetupId(1), .. })
        ));
    }

    #[test]
    fn alias_merge_blocks_across_brackets() {
        // Same human: player 20 in melee, 900 in mugen (aliased).
        let world = World::new();
        let aliases = AliasMap::build(&[vec![player("20"), player("900")]]);

        let melee_id = BracketId("melee".to_owned());
        let mugen_id = BracketId("mugen".to_owned());
        let mut melee_sets = [simple_set("77", "A", ["20", "21"])];
        let mugen_sets = [simple_set("88", "A", ["900", "901"])];
        melee_sets[0].started_at = Some(NOW / 1000 - 60);

        let pool = vec![SetupId(1), SetupId(2)];
        let views = [
            BracketView {
                id: &melee_id,
                sets: &melee_sets,
                mode: BracketMode::Full,
                start_at: None,
                held: false,
                pool: &pool,
            },
            BracketView {
                id: &mugen_id,
                sets: &mugen_sets,
                mode: BracketMode::Full,
                start_at: None,
                held: false,
                pool: &pool,
            },
        ];
        let mut inputs = world.inputs();
        inputs.aliases = &aliases;
        let index = ConflictIndex::build(&views, &inputs);

        let reasons = callable(&views[1], &mugen_sets[0], &index, &inputs, NOW).unwrap_err();
        assert_eq!(
            reasons,
            vec![BlockReason::PlayerBusy {
                key: ConflictKey::Player(player("20")),
                source: BusySource::RemoteActive {
                    bracket: melee_id.clone(),
                    set: melee_sets[0].key.clone(),
                },
            }],
            "canonical key is the smaller id (20)"
        );
    }

    #[test]
    fn conflict_only_feeds_busy_but_never_emerges() {
        let world = World::new();
        let ladder_id = BracketId("ladder".to_owned());
        let mut ladder_sets = [simple_set("99", "A", ["10", "50"])];
        ladder_sets[0].started_at = Some(NOW / 1000 - 60);
        let melee_sets = [simple_set("77", "A", ["10", "11"])];

        let pool = vec![SetupId(1)];
        let views = [
            BracketView {
                id: &ladder_id,
                sets: &ladder_sets,
                mode: BracketMode::ConflictOnly,
                start_at: None,
                held: false,
                pool: &pool,
            },
            world.view(&melee_sets),
        ];
        let inputs = world.inputs();
        let index = ConflictIndex::build(&views, &inputs);

        // The ladder set blocks melee through player 10...
        let reasons = callable(&views[1], &melee_sets[0], &index, &inputs, NOW).unwrap_err();
        assert!(matches!(&reasons[0], BlockReason::PlayerBusy { key, .. } if *key == key_of("10")));

        // ...and nothing from the ladder is ever callable.
        let all = callable_sets(&views, &index, &inputs, NOW);
        assert!(all.iter().all(|c| c.bracket != ladder_id));
    }

    #[test]
    fn identity_degraded_serializes_within_event_only() {
        let world = World::new();
        // Two sets in one event share a degraded entrant (no player ids);
        // in another event the same human holds a different entrant id.
        let degraded = || occupant("e-mystery", &[]);
        let mut melee_sets = [
            set_between("77", "A", degraded(), occupant("e-b", &["30"])),
            set_between("77", "B", degraded(), occupant("e-c", &["31"])),
        ];
        melee_sets[0].started_at = Some(NOW / 1000 - 60);
        let mugen_id = BracketId("mugen".to_owned());
        let mugen_sets = [set_between("88", "A", occupant("e-other", &[]), occupant("e-d", &["32"]))];

        let views = [
            world.view(&melee_sets),
            BracketView {
                id: &mugen_id,
                sets: &mugen_sets,
                mode: BracketMode::Full,
                start_at: None,
                held: false,
                pool: &world.pool,
            },
        ];
        let inputs = world.inputs();
        let index = ConflictIndex::build(&views, &inputs);

        let reasons = callable(&views[0], &melee_sets[1], &index, &inputs, NOW).unwrap_err();
        assert_eq!(
            reasons,
            vec![BlockReason::PlayerBusy {
                key: ConflictKey::Entrant(EntrantId("e-mystery".to_owned())),
                source: BusySource::RemoteActive {
                    bracket: world.bracket.clone(),
                    set: melee_sets[0].key.clone(),
                },
            }],
            "within the event the entrant key serializes"
        );
        assert!(
            callable(&views[1], &mugen_sets[0], &index, &inputs, NOW).is_ok(),
            "nothing carries to the other event (accepted degradation)"
        );
    }

    #[test]
    fn rest_window_blocks_until_expiry() {
        let mut world = World::new();
        world.rest_window_secs = 300;
        world.last_completed.insert(key_of("10"), NOW - 100_000);
        let sets = [simple_set("77", "A", ["10", "11"])];

        let reasons = world.check(&sets, 0).unwrap_err();
        assert_eq!(
            reasons,
            vec![BlockReason::RestWindow {
                key: key_of("10"),
                until: NOW + 200_000,
            }]
        );

        world.last_completed.insert(key_of("10"), NOW - 400_000);
        assert!(world.check(&sets, 0).is_ok(), "window expired");
    }

    #[test]
    fn force_available_suppresses_remote_but_not_local() {
        let mut world = World::new();
        let mut sets = [simple_set("77", "A", ["10", "11"]), simple_set("77", "B", ["10", "12"])];
        sets[0].started_at = Some(NOW / 1000 - 60);
        world.flags.force_available.insert(key_of("10"));

        assert!(world.check(&sets, 1).is_ok(), "remote evidence suppressed");

        world.board.set_status(
            SetupId(1),
            SetupStatus::Called {
                bracket: world.bracket.clone(),
                set: sets[0].key.clone(),
            },
        );
        let reasons = world.check(&sets, 1).unwrap_err();
        assert!(
            matches!(
                &reasons[0],
                BlockReason::PlayerBusy {
                    source: BusySource::LocalSetup { .. },
                    ..
                }
            ),
            "our own call is never suppressed: {reasons:?}"
        );
    }

    #[test]
    fn pool_restricts_candidate_setups() {
        let mut world = World::new();
        world.pool = vec![SetupId(2)];
        let sets = [simple_set("77", "A", ["10", "11"])];

        let callable = world.check(&sets, 0).unwrap();
        assert_eq!(callable.candidate_setups, vec![SetupId(2)]);

        world.board.set_status(SetupId(2), SetupStatus::OccupiedExternal { set: None });
        let reasons = world.check(&sets, 0).unwrap_err();
        assert_eq!(
            reasons,
            vec![BlockReason::NoPermittedFreeSetup],
            "setup 1 is free but not permitted"
        );
    }

    #[test]
    fn occupied_external_with_tracked_set_marks_players_busy() {
        let mut world = World::new();
        let sets = [simple_set("77", "A", ["10", "11"]), simple_set("77", "B", ["10", "12"])];
        world.board.set_status(
            SetupId(1),
            SetupStatus::OccupiedExternal {
                set: Some((world.bracket.clone(), sets[0].key.clone())),
            },
        );
        let reasons = world.check(&sets, 1).unwrap_err();
        assert!(matches!(
            &reasons[0],
            BlockReason::PlayerBusy {
                source: BusySource::LocalSetup { .. },
                ..
            }
        ));
    }

    #[test]
    fn soft_deviation_escalation_is_caller_gated() {
        let mut world = World::new();
        let sets = [simple_set("77", "A", ["10", "11"]), simple_set("77", "B", ["10", "12"])];
        assert!(world.check(&sets, 1).is_ok());

        world.soft_busy.push((world.bracket.clone(), sets[0].key.clone()));
        let reasons = world.check(&sets, 1).unwrap_err();
        assert!(matches!(
            &reasons[0],
            BlockReason::PlayerBusy {
                source: BusySource::SoftDeviation { .. },
                ..
            }
        ));
    }

    #[test]
    fn state_deviation_flags_only_unknown_changes() {
        let known = [1, 2, 3, 6];
        let baseline = simple_set("77", "A", ["10", "11"]);
        let mut set = baseline.clone();

        assert_eq!(state_deviation(&set, &baseline, &known), None, "no change");

        set.state_int = Some(6);
        assert_eq!(state_deviation(&set, &baseline, &known), None, "known int");

        set.state_int = Some(99);
        let deviation = state_deviation(&set, &baseline, &known).expect("unknown int flags");
        assert_eq!(deviation.from, Some(1));
        assert_eq!(deviation.to, Some(99));
    }

    #[test]
    fn aggregate_remaining_merges_aliases_across_brackets() {
        let aliases = AliasMap::build(&[vec![player("P1"), player("P1-alt")]]);
        let melee = crate::synth::make_de_bracket(9, 4);
        let mut mugen = crate::synth::make_de_bracket(8, 4);
        for set in &mut mugen.sets {
            for slot in &mut set.slots {
                if let Some(o) = &mut slot.occupant {
                    if o.player_ids[0] == player("P1") {
                        o.player_ids = vec![player("P1-alt")];
                    }
                }
            }
        }

        let melee_id = BracketId("melee".to_owned());
        let mugen_id = BracketId("mugen".to_owned());
        let (melee_graph, _) = BracketGraph::build(&melee.sets, &[melee.info]);
        let (mugen_graph, _) = BracketGraph::build(&mugen.sets, &[mugen.info]);

        let totals = aggregate_remaining(&[(&melee_id, &melee_graph), (&mugen_id, &mugen_graph)], &aliases);
        assert_eq!(totals[&key_of("P1")], 2, "one R1 set in each bracket, merged");
    }

    #[test]
    fn alias_map_picks_smallest_id_and_handles_overlap() {
        let aliases = AliasMap::build(&[vec![player("900"), player("20")], vec![player("900"), player("1000")]]);
        for id in ["900", "20", "1000"] {
            assert_eq!(aliases.canonical(&player(id)), player("20"), "transitive merge to smallest");
        }
        assert_eq!(aliases.canonical(&player("7")), player("7"), "unmapped ids are their own canonical");
    }

    #[test]
    fn occupant_keys_fall_back_to_entrant() {
        let aliases = AliasMap::default();
        assert_eq!(
            occupant_keys(&occupant("e-1", &["10", "11"]), &aliases),
            vec![key_of("10"), key_of("11")],
            "doubles: every participant is a key"
        );
        assert_eq!(
            occupant_keys(&occupant("e-1", &[]), &aliases),
            vec![ConflictKey::Entrant(EntrantId("e-1".to_owned()))]
        );
    }

    #[test]
    fn setup_board_reports_free_setups() {
        let mut board = SetupBoard::new(&[SetupId(1), SetupId(2), SetupId(3)]);
        board.set_status(SetupId(2), SetupStatus::OccupiedExternal { set: None });
        assert_eq!(board.free_ids().collect::<Vec<_>>(), vec![SetupId(1), SetupId(3)]);
        assert_eq!(
            board.setups()[1],
            Setup {
                id: SetupId(2),
                status: SetupStatus::OccupiedExternal { set: None },
                setup_type: "default".to_owned(),
            }
        );
    }

    fn typed_board(roster: &[(u32, &str)]) -> SetupBoard {
        SetupBoard::from_roster(&roster.iter().map(|(id, t)| (SetupId(*id), (*t).to_owned())).collect::<Vec<_>>())
    }

    #[test]
    fn board_add_retire_and_number_reuse() {
        let mut board = typed_board(&[(1, "switch"), (2, "switch"), (3, "pokemon")]);
        assert_eq!(board.lowest_unused_id(), SetupId(4));

        board.remove_setup(SetupId(2));
        assert_eq!(board.lowest_unused_id(), SetupId(2), "a retired number is reused");
        board.add_setup(SetupId(2), "pokemon".to_owned());
        let ids: Vec<u32> = board.setups().iter().map(|s| s.id.0).collect();
        assert_eq!(ids, vec![1, 2, 3], "the board stays in id order");
        assert_eq!(board.setups()[1].setup_type, "pokemon", "the reused number carries its new type");

        let counts = board.counts_by_type();
        assert_eq!(counts["switch"], 1);
        assert_eq!(counts["pokemon"], 2);
    }

    #[test]
    fn effective_pool_matches_types_and_folds_overrides() {
        let board = typed_board(&[(1, "switch"), (2, "switch"), (3, "pokemon")]);
        let melee = BracketId("melee".to_owned());
        let pokemon = BracketId("pokemon".to_owned());
        let switch_only = ["switch".to_owned()];
        let both = ["switch".to_owned(), "pokemon".to_owned()];

        let no_overrides = HashMap::new();
        assert_eq!(
            effective_pool(&melee, &switch_only, board.setups(), &no_overrides),
            vec![SetupId(1), SetupId(2)]
        );
        assert_eq!(
            effective_pool(&melee, &both, board.setups(), &no_overrides),
            vec![SetupId(1), SetupId(2), SetupId(3)],
            "a type list unions the pools"
        );

        // Dedicating a matching-type station to someone else removes it; a
        // dedication to us adds a station our types would never match.
        let overrides = HashMap::from([
            (SetupId(2), PoolOverride::Dedicated(pokemon.clone())),
            (SetupId(3), PoolOverride::Dedicated(melee.clone())),
        ]);
        assert_eq!(
            effective_pool(&melee, &switch_only, board.setups(), &overrides),
            vec![SetupId(1), SetupId(3)]
        );

        let overrides = HashMap::from([(SetupId(3), PoolOverride::AllowAny)]);
        assert_eq!(
            effective_pool(&melee, &switch_only, board.setups(), &overrides),
            vec![SetupId(1), SetupId(2), SetupId(3)],
            "AllowAny opens a foreign-type station to everyone"
        );
    }
}

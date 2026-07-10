//! The Elm core: [`AppState`], [`Msg`], and the pure synchronous [`update`].
//!
//! `update` performs no I/O and reads no clocks — the caller passes `now` and
//! side effects come back as [`UpdateEffects`] requests (writes to enqueue,
//! events to force-poll). Everything here is driven the same way by the real
//! main loop and by tests.

use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet, VecDeque},
    mem::discriminant,
};

use bracket_tools_startgg::{CharacterInfo, GameReport, GameSelection, SetMutationResult, StartGgId};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Serialize};

use crate::{
    config::{referenced_types, resolve_roster, BracketMode, SchedulerConfig, SetupId, FALLBACK_SETUPS_PER_TYPE},
    conflict::{
        callable, effective_pool, occupant_keys, state_deviation, AliasMap, BlockReason, BracketView, CallableSet, ConflictIndex,
        ConflictInputs, ConflictKey, PlayerFlags, PoolOverride, SetupBoard, SetupStatus, Tombstones, UnixMillis,
    },
    duration::{diff_snapshots, DurationModel},
    model::{BracketId, LiveSet, ModelWarning, PhaseGroupInfo, SetKey, SkippedSet},
    persist::{BracketSnapshot, OverlayDoc, SnapshotDoc, OVERLAY_VERSION, SNAPSHOT_VERSION},
    ranker::GreedyRanker,
    world::{assigned_sets, recompute, BracketState, RolloutRankings, RolloutRow, SimSnapshot, World, WorldInputs},
};

/// How long `z` parks a queue entry.
pub const SNOOZE_SECS: i64 = 300;
const NOTICE_CAP: usize = 200;
/// Idle refresh cadence for the time-derived world terms (rest windows,
/// snooze expiry, wait credit). A recompute runs a full forward sim, so a
/// bare tick only triggers one when the world is at least this stale;
/// anything that actually changes state recomputes immediately.
pub const WORLD_REFRESH_MS: i64 = 10_000;

#[derive(Debug)]
pub enum Msg {
    Key(KeyEvent),
    Poll(PollResult),
    Write(WriteResult),
    /// A background rollout evaluation landed.
    SimResult(RolloutRankings),
    /// 1s display tick; also re-runs the recompute so time-gated state
    /// (snoozes, rest windows, bracket open times) stays current.
    Tick,
}

/// How urgently the background simulator should re-evaluate after an update.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SimUrgency {
    /// Debounced (≥5s between evaluations): routine state drift.
    Routine,
    /// The decision-point exemption: a setup freed — evaluate the post-free
    /// world immediately so the next call-picker opens on a rollout ranking.
    Immediate,
}

#[derive(Debug)]
pub struct PollResult {
    pub bracket: BracketId,
    /// Global monotonic cycle stamp; snapshots apply only in order.
    pub seq: u64,
    pub captured_at: UnixMillis,
    pub outcome: PollOutcome,
}

#[derive(Debug)]
pub enum PollOutcome {
    Snapshot {
        sets: Vec<LiveSet>,
        warnings: Vec<ModelWarning>,
        skipped: Vec<SkippedSet>,
    },
    Failed(PollFailure),
}

/// The three-bucket read-error classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollFailure {
    /// Connectivity (connect/timeout): likely offline, retry silently.
    Offline,
    /// Transient server trouble (429/5xx): failed cycle, retry next tick.
    Transient,
    /// Persistent request problem (other 4xx): banner until it changes.
    Persistent(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WriteKind {
    Called,
    InProgress,
    Report(Box<ReportPayload>),
}

impl WriteKind {
    /// Short label for notices (the `Report` payload is too big to print).
    pub fn label(&self) -> &'static str {
        match self {
            WriteKind::Called => "Called",
            WriteKind::InProgress => "InProgress",
            WriteKind::Report(_) => "Report",
        }
    }
}

/// Everything `reportBracketSet` needs, carried on the write intent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportPayload {
    pub winner_entrant_id: Option<String>,
    pub is_dq: bool,
    pub games: Vec<GameReport>,
    /// Human-readable result ("A 2-1 B") for notices.
    pub summary: String,
}

/// A mutation the update loop wants performed. The writer task owns retries;
/// the intent is immutable once issued.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WriteIntent {
    pub bracket: BracketId,
    pub key: SetKey,
    pub id: StartGgId,
    pub kind: WriteKind,
    pub created_at: UnixMillis,
}

#[derive(Debug)]
pub struct WriteResult {
    pub intent: WriteIntent,
    pub outcome: WriteOutcome,
}

#[derive(Debug)]
pub enum WriteOutcome {
    Success {
        payload: SetMutationResult,
        /// Server-clock offset observed on this round trip, when the payload
        /// carried a server timestamp.
        offset: Option<OffsetSample>,
    },
    /// Still being retried by the writer; informational.
    Transient { error: String, attempts: u32 },
    /// Connectivity failure: the writer hands the intent back; the app holds
    /// it until the target event polls successfully again, then revalidates
    /// and re-sends (or drops a moot intent). Never consumes park attempts.
    AwaitReconnect { error: String },
    /// Given up; parked for the TO to retry or discard.
    Terminal { error: String },
}

/// One server-clock offset observation, bracketed from a mutation round trip
/// (`started_at` is stamped by the mutation itself, so server time minus the
/// local send/receive midpoint is one-RTT tight).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OffsetSample {
    /// `server_seconds - local_seconds` at the sample instant.
    pub offset_secs: i64,
    /// When the sample was taken (local millis) — drives the status-line age.
    pub at: UnixMillis,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PendingStatus {
    Queued,
    /// Failed on connectivity; held until its event polls successfully again
    /// (the flush discipline — revalidated before re-sending).
    AwaitingReconnect,
    Parked,
}

/// A write the TO committed locally that hasn't been confirmed remotely.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingWrite {
    pub intent: WriteIntent,
    pub status: PendingStatus,
    pub attempts: u32,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NoticeLevel {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Notice {
    pub at: UnixMillis,
    pub level: NoticeLevel,
    pub text: String,
    /// Cleared by the notices page (`n`). Unacked `Warn`/`Error` notices are
    /// the correctness-relevant ones persisted across a restart.
    #[serde(default)]
    pub acked: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Modal {
    /// Candidate list for one free setup; Enter calls the selected set.
    CallPicker {
        setup: SetupId,
        selected: usize,
        /// One rollout refresh already entered this modal session; later
        /// results wait until it closes ("ranking updated" flag).
        refreshed: bool,
    },
    /// Why-not-callable browser over `world.blocked` (`i`).
    Inspection {
        selected: usize,
    },
    /// Scrollable notices ring, newest first; Enter acks (`n`).
    Notices {
        selected: usize,
    },
    /// Pending/parked writes + the divergence ledger; Enter retries a parked
    /// entry, `d` discards it (`w`).
    PendingWrites {
        selected: usize,
    },
    /// Tri-state player flags for the highlighted queue entry's players;
    /// Enter cycles resting → departed → force-available → clear (`d`).
    PlayerFlags {
        players: Vec<(ConflictKey, String)>,
        selected: usize,
    },
    /// Reassign the selected setup's pool: dedicate to one bracket, open to
    /// all, or restore the config pools (`a`).
    Reassign {
        setup: SetupId,
        selected: usize,
    },
    /// Add/retire stations mid-event (`s`). Stays open across edits so a
    /// batch of arrivals is a few Enters.
    Setups {
        selected: usize,
    },
    /// Game-by-game set reporting for the selected setup's set (`g`).
    Report(Box<ReportDraft>),
    Help,
}

/// The in-flight report the modal edits. Winner taps are the hot path (`1`/
/// `2` per game); characters are optional and per game — a new game copies
/// the previous game's picks, and editing a game re-propagates from it
/// forward. Sticky picks per player seed the first game across sets.
#[derive(Debug, Clone, PartialEq)]
pub struct ReportDraft {
    /// The setup the set is on (freed on submit).
    pub setup: SetupId,
    pub bracket: BracketId,
    pub key: SetKey,
    pub raw_id: String,
    pub left: ReportSide,
    pub right: ReportSide,
    pub best_of: Option<i32>,
    /// Each game so far, in play order.
    pub games: Vec<GameDraft>,
    /// Character picks (left, right) for the first game before it is
    /// recorded; later games copy their predecessor.
    pub chars: [Option<i32>; 2],
    /// The game the character picker targets (picks apply from it onward);
    /// Up/Down move it in the Games stage.
    pub game_cursor: usize,
    pub stage: ReportStage,
}

/// One recorded game: its winner and both sides' character picks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GameDraft {
    pub winner: Side,
    pub chars: [Option<i32>; 2],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

impl Side {
    pub fn ix(self) -> usize {
        match self {
            Side::Left => 0,
            Side::Right => 1,
        }
    }

    fn other(self) -> Side {
        match self {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReportSide {
    pub entrant_id: String,
    pub name: String,
    /// Sticky-character key: the first player id, else the entrant id (stable
    /// across events for the same human).
    pub sticky_key: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ReportStage {
    /// `1`/`2` record a game winner; `c` characters, `d` DQ, Enter finish.
    Games,
    /// Prefix-search character picker for one side.
    Characters { side: Side, filter: String, cursor: usize },
    /// `1`/`2` choose which side is disqualified.
    DqPick,
    /// Summary + `y` to submit.
    Confirm { dq: Option<Side> },
}

impl ReportDraft {
    pub fn wins(&self, side: Side) -> usize {
        self.games.iter().filter(|g| g.winner == side).count()
    }

    pub fn side(&self, side: Side) -> &ReportSide {
        match side {
            Side::Left => &self.left,
            Side::Right => &self.right,
        }
    }

    /// The side with strictly more game wins, if any.
    pub fn leader(&self) -> Option<Side> {
        match self.wins(Side::Left).cmp(&self.wins(Side::Right)) {
            Ordering::Greater => Some(Side::Left),
            Ordering::Less => Some(Side::Right),
            Ordering::Equal => None,
        }
    }

    /// True once a side has the majority of a known best-of.
    fn clinched(&self) -> bool {
        let Some(best_of) = self.best_of else { return false };
        let needed = (best_of as usize) / 2 + 1;
        self.wins(Side::Left) >= needed || self.wins(Side::Right) >= needed
    }

    /// "A 2-1 B" (winner first), or the DQ phrasing.
    pub fn summary(&self, dq: Option<Side>) -> String {
        if let Some(dq_side) = dq {
            let winner = self.side(dq_side.other());
            return format!("{} wins by DQ over {}", winner.name, self.side(dq_side).name);
        }
        let (winner, loser) = match self.leader() {
            Some(Side::Right) => (Side::Right, Side::Left),
            _ => (Side::Left, Side::Right),
        };
        format!(
            "{} {}-{} {}",
            self.side(winner).name,
            self.wins(winner),
            self.wins(loser),
            self.side(loser).name
        )
    }
}

/// One choice in the reassign modal. Built deterministically so the render
/// and the commit agree on indexing.
#[derive(Debug, Clone, PartialEq)]
pub enum ReassignOption {
    Dedicate(BracketId),
    AllowAny,
    RestoreConfig,
}

/// The reassign modal's option list: every full bracket, then the two
/// blanket choices.
pub(crate) fn reassign_options(state: &AppState) -> Vec<ReassignOption> {
    let mut options: Vec<ReassignOption> = state
        .brackets
        .iter()
        .filter(|b| b.state.mode == BracketMode::Full)
        .map(|b| ReassignOption::Dedicate(b.state.id.clone()))
        .collect();
    options.push(ReassignOption::AllowAny);
    options.push(ReassignOption::RestoreConfig);
    options
}

/// One row of the setups modal. Built deterministically so the render and
/// the commit agree on indexing.
#[derive(Debug, Clone, PartialEq)]
pub enum SetupsRow {
    /// An existing station (with its type), retired on Enter when free.
    Retire(SetupId, String),
    /// "Add a station" for one type.
    Add(String),
}

/// The setups modal's rows: stations grouped by type (board order), each
/// group closed by its add row. Config-referenced types with no station yet
/// still get an add row, so a zero-count type can be seeded.
pub(crate) fn setups_rows(state: &AppState) -> Vec<SetupsRow> {
    let mut types: Vec<String> = Vec::new();
    for setup in state.board.setups() {
        if !types.contains(&setup.setup_type) {
            types.push(setup.setup_type.clone());
        }
    }
    for referenced in referenced_types(&state.config) {
        if !types.contains(&referenced) {
            types.push(referenced);
        }
    }

    let mut rows = Vec::new();
    for setup_type in types {
        for setup in state.board.setups().iter().filter(|s| s.setup_type == setup_type) {
            rows.push(SetupsRow::Retire(setup.id, setup_type.clone()));
        }
        rows.push(SetupsRow::Add(setup_type));
    }
    rows
}

#[derive(Debug, Clone, Default)]
pub struct UiState {
    pub modal: Option<Modal>,
    /// Occupied setup the hot keys (`p`/`f`/`r`) act on.
    pub selected_setup: Option<SetupId>,
    /// Cursor into `world.queue` (snooze target / inspection).
    pub queue_ix: usize,
}

/// Per-bracket poll bookkeeping around the recompute-facing [`BracketState`].
#[derive(Debug, Clone)]
pub struct BracketRuntime {
    pub state: BracketState,
    /// The event's character roster (reporting vocabulary; empty = none).
    pub characters: Vec<CharacterInfo>,
    pub applied_seq: u64,
    pub last_good_poll: Option<UnixMillis>,
    pub consecutive_failures: u32,
    pub health: PollHealth,
    /// Tearing guard: sets that vanished from the last successful snapshot,
    /// carried forward one grace cycle before being dropped for real (a torn
    /// paginated fetch can miss a set that still exists).
    suspects: HashSet<SetKey>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollHealth {
    Ok,
    Offline,
    Transient,
    Persistent(String),
}

/// What preflight hands the app per configured bracket.
#[derive(Debug, Clone)]
pub struct BracketBootstrap {
    pub id: BracketId,
    pub sets: Vec<LiveSet>,
    pub groups: Vec<PhaseGroupInfo>,
    pub mode: crate::config::BracketMode,
    /// Effective open time (config override already folded in), unix secs.
    pub start_at: Option<i64>,
    pub setup_types: Vec<String>,
    pub duration_prior_secs: u64,
    pub prior_weight: f64,
    /// The event's character roster (may be empty).
    pub characters: Vec<CharacterInfo>,
}

/// Side effects `update` wants performed. The main loop translates these
/// into channel sends; tests assert on them directly.
#[derive(Debug, Default)]
pub struct UpdateEffects {
    pub writes: Vec<WriteIntent>,
    /// Events whose next poll should happen immediately (freed setups
    /// awaiting results).
    pub force_poll: Vec<BracketId>,
    /// Ask the background simulator for a fresh rollout evaluation.
    pub sim: Option<SimUrgency>,
    pub quit: bool,
}

impl UpdateEffects {
    /// Folds another update's effects in (drain-then-draw coalescing).
    pub fn merge(&mut self, other: Self) {
        self.writes.extend(other.writes);
        self.force_poll.extend(other.force_poll);
        self.sim = self.sim.max(other.sim);
        self.quit |= other.quit;
    }

    fn want_sim(&mut self, urgency: SimUrgency) {
        self.sim = self.sim.max(Some(urgency));
    }
}

/// Undo is single-level and local: it restores the overlay, not writes
/// already handed to the writer.
#[derive(Debug, Clone)]
struct UndoSnapshot {
    board: SetupBoard,
    tombstones: Tombstones,
    flags: PlayerFlags,
    pool_overrides: HashMap<SetupId, PoolOverride>,
    snoozes: HashMap<(BracketId, SetKey), UnixMillis>,
    called_at: HashMap<(BracketId, SetKey), UnixMillis>,
    description: String,
}

pub struct AppState {
    pub config: SchedulerConfig,
    /// Writes reach the network only when armed (admin probe passed AND no
    /// advisor-only override). Local board state works either way.
    pub writes_armed: bool,
    pub brackets: Vec<BracketRuntime>,

    // The in-memory overlay (S4 persists parts of this).
    pub board: SetupBoard,
    pub flags: PlayerFlags,
    pub tombstones: Tombstones,
    /// Per-setup pool reassignments (the `a` action).
    pub pool_overrides: HashMap<SetupId, PoolOverride>,
    pub aliases: AliasMap,
    pub snoozes: HashMap<(BracketId, SetKey), UnixMillis>,
    pub last_completed: HashMap<ConflictKey, UnixMillis>,
    /// When each set first became ready (slots filled) — wait-time credit.
    pub callable_since: HashMap<SetKey, UnixMillis>,
    /// When the TO called each set locally (no-show timer, duration ingest).
    pub called_at: HashMap<(BracketId, SetKey), UnixMillis>,
    /// Sticky character memory: last pick per player (reporting seeds from
    /// this, so regulars only pick once a day). Persisted in the overlay.
    pub last_characters: HashMap<String, i32>,
    pub called_ints: Vec<i32>,
    pub in_progress_ints: Vec<i32>,
    pub soft_busy: Vec<(BracketId, SetKey)>,
    pub durations: DurationModel,
    pub pending_writes: Vec<PendingWrite>,
    /// Intents awaiting reconnect (in-memory twin of the AwaitingReconnect
    /// pending entries): released back to the writer only once their event
    /// polls successfully, strictly newer than the intent.
    pub held_writes: Vec<WriteIntent>,
    pub notices: VecDeque<Notice>,
    /// Latest server-clock offset estimate (from mutation round trips);
    /// retained until a fresher sample replaces it. Not persisted — clocks
    /// drift and a restart re-estimates on the first write.
    pub clock_offset: Option<OffsetSample>,
    no_show_alerted: HashSet<(BracketId, SetKey)>,

    pub world: World,
    /// Latest background rollout evaluation (the call-picker's preferred
    /// ranking source; greedy is the fallback and the permanent revert).
    pub rollout: Option<RolloutRankings>,
    /// A result held back by the one-refresh-per-open-modal policy; applied
    /// when the picker closes.
    rollout_pending: Option<RolloutRankings>,
    pub dirty: bool,
    /// When the world was last recomputed; bare ticks skip the recompute
    /// until [`WORLD_REFRESH_MS`] has passed.
    last_recompute: UnixMillis,
    /// Set whenever a message may have changed the persisted overlay; the main
    /// loop debounces a save and clears it.
    pub overlay_dirty: bool,
    /// Set when a snapshot applied (the last-good tables changed); the main
    /// loop debounces the snapshot-file save and clears it.
    pub snapshot_dirty: bool,
    /// The last overlay save failed (state file unwritable) — drives the
    /// "STATE NOT PERSISTING" badge.
    pub persist_failed: bool,
    pub ui: UiState,
    undo: Option<UndoSnapshot>,
}

impl AppState {
    pub fn new(config: SchedulerConfig, writes_armed: bool, bootstraps: Vec<BracketBootstrap>, now_millis: UnixMillis) -> Self {
        let mut durations = DurationModel::new();
        let brackets: Vec<BracketRuntime> = bootstraps
            .into_iter()
            .map(|b| {
                durations.configure_bracket(b.id.clone(), b.duration_prior_secs, b.prior_weight);
                BracketRuntime {
                    state: BracketState {
                        id: b.id,
                        sets: b.sets,
                        groups: b.groups,
                        mode: b.mode,
                        start_at: b.start_at,
                        held: false,
                        setup_types: b.setup_types,
                    },
                    characters: b.characters,
                    applied_seq: 0,
                    last_good_poll: Some(now_millis),
                    consecutive_failures: 0,
                    health: PollHealth::Ok,
                    suspects: HashSet::new(),
                }
            })
            .collect();

        let roster = resolve_roster(&config);
        let mut state = Self {
            board: SetupBoard::from_roster(&roster.roster),
            aliases: AliasMap::build(&config.player_aliases),
            called_ints: config.known_called_state_int.into_iter().collect(),
            in_progress_ints: config.known_in_progress_state_int.into_iter().collect(),
            writes_armed,
            brackets,
            flags: PlayerFlags::default(),
            tombstones: Tombstones::default(),
            pool_overrides: HashMap::new(),
            snoozes: HashMap::new(),
            last_completed: HashMap::new(),
            callable_since: HashMap::new(),
            called_at: HashMap::new(),
            last_characters: HashMap::new(),
            soft_busy: Vec::new(),
            durations,
            pending_writes: Vec::new(),
            held_writes: Vec::new(),
            notices: VecDeque::new(),
            clock_offset: None,
            no_show_alerted: HashSet::new(),
            world: World::default(),
            rollout: None,
            rollout_pending: None,
            dirty: false,
            last_recompute: 0,
            overlay_dirty: false,
            snapshot_dirty: false,
            persist_failed: false,
            ui: UiState::default(),
            undo: None,
            config,
        };
        if roster.fallback {
            let text =
                format!("no setup counts configured — assuming {FALLBACK_SETUPS_PER_TYPE} per type (fix with --setups or the config)");
            state.notice(now_millis, NoticeLevel::Warn, text);
        }
        for setup_type in &roster.zero_station_types {
            let text = format!("setup type {setup_type:?} has no stations — add some with 's'");
            state.notice(now_millis, NoticeLevel::Warn, text);
        }
        for ix in 0..state.brackets.len() {
            stamp_ready(&mut state, ix, now_millis);
        }
        state.world = recompute_world(&state, now_millis);
        state
    }

    pub fn notice(&mut self, at: UnixMillis, level: NoticeLevel, text: impl Into<String>) {
        self.notices.push_back(Notice {
            at,
            level,
            text: text.into(),
            acked: false,
        });
        if self.notices.len() > NOTICE_CAP {
            self.notices.pop_front();
        }
    }

    /// Snapshots the persisted overlay. Only unacked `Warn`/`Error` notices
    /// survive — `Info` chatter ("called X", "freed Y") is transient, not
    /// correctness state worth rehydrating.
    pub fn to_overlay(&self) -> OverlayDoc {
        OverlayDoc {
            version: OVERLAY_VERSION,
            board: self.board.clone(),
            flags: self.flags.clone(),
            tombstones: self.tombstones.clone(),
            pool_overrides: self.pool_overrides.iter().map(|(s, o)| (*s, o.clone())).collect(),
            snoozes: flatten_pair_map(&self.snoozes),
            last_completed: self.last_completed.iter().map(|(k, v)| (k.clone(), *v)).collect(),
            callable_since: self.callable_since.iter().map(|(k, v)| (k.clone(), *v)).collect(),
            called_at: flatten_pair_map(&self.called_at),
            last_characters: self.last_characters.iter().map(|(k, v)| (k.clone(), *v)).collect(),
            called_ints: self.called_ints.clone(),
            in_progress_ints: self.in_progress_ints.clone(),
            soft_busy: self.soft_busy.clone(),
            durations: self.durations.clone(),
            pending_writes: self.pending_writes.clone(),
            notices: self
                .notices
                .iter()
                .filter(|n| !n.acked && n.level != NoticeLevel::Info)
                .cloned()
                .collect(),
            no_show_alerted: self.no_show_alerted.iter().cloned().collect(),
        }
    }

    /// Rehydrates a persisted overlay over a freshly-bootstrapped state,
    /// reconciling against the current config (known state ints stay
    /// config-authoritative) and recomputing the world.
    ///
    /// The station roster: a typed persisted board is the operator's last
    /// known station reality, so it's adopted wholesale when `adopt_roster`
    /// (i.e. `--setups` didn't pin counts this run). A pre-migration board
    /// (untyped `setup_type`s) or a pinned run instead re-keys statuses by id
    /// onto the freshly-resolved roster, preserving crash recovery.
    pub fn apply_overlay(&mut self, doc: OverlayDoc, now_millis: UnixMillis, adopt_roster: bool) {
        let pre_migration = doc.board.setups().iter().any(|s| s.setup_type.is_empty());
        if adopt_roster && !pre_migration && !doc.board.setups().is_empty() {
            self.board = doc.board.clone();
            let referenced = referenced_types(&self.config);
            let mut unreferenced: Vec<&str> = self
                .board
                .setups()
                .iter()
                .map(|s| s.setup_type.as_str())
                .filter(|t| !referenced.iter().any(|r| r == t))
                .collect();
            unreferenced.dedup();
            if !unreferenced.is_empty() {
                let text = format!(
                    "restored roster has setup type(s) no bracket references: {} (retire with 's' if stale)",
                    unreferenced.join(", ")
                );
                self.notice(now_millis, NoticeLevel::Warn, text);
            }
        } else {
            let known_ids: HashSet<SetupId> = self.board.setups().iter().map(|s| s.id).collect();
            for setup in doc.board.setups() {
                if known_ids.contains(&setup.id) {
                    self.board.set_status(setup.id, setup.status.clone());
                } else {
                    self.notice(
                        now_millis,
                        NoticeLevel::Warn,
                        format!("dropped persisted state for setup {} (not in config)", setup.id.0),
                    );
                }
            }
        }
        // A status naming a bracket outside this config is another
        // tournament's leftovers (the shared XDG state file): free it.
        self.reset_unknown_bracket_statuses(now_millis);

        self.flags = doc.flags;
        self.tombstones = doc.tombstones;
        let known_ids: HashSet<SetupId> = self.board.setups().iter().map(|s| s.id).collect();
        for (setup, over) in doc.pool_overrides {
            let known_bracket = match &over {
                PoolOverride::Dedicated(b) => self.bracket_ix(b).is_some(),
                PoolOverride::AllowAny => true,
            };
            if known_ids.contains(&setup) && known_bracket {
                self.pool_overrides.insert(setup, over);
            } else {
                self.notice(
                    now_millis,
                    NoticeLevel::Warn,
                    format!("dropped persisted pool override for setup {} (no longer applies)", setup.0),
                );
            }
        }
        self.snoozes = doc.snoozes.into_iter().map(|(b, k, v)| ((b, k), v)).collect();
        self.last_completed = doc.last_completed.into_iter().collect();
        self.callable_since = doc.callable_since.into_iter().collect();
        self.called_at = doc.called_at.into_iter().map(|(b, k, v)| ((b, k), v)).collect();
        self.last_characters = doc.last_characters.into_iter().collect();
        // Union so a config pin added since the last run is never dropped.
        merge_ints(&mut self.called_ints, doc.called_ints);
        merge_ints(&mut self.in_progress_ints, doc.in_progress_ints);
        self.soft_busy = doc.soft_busy;
        self.durations.restore(doc.durations);
        // A write left in flight (or held for reconnect) when we crashed has
        // an uncertain fate; park it for the TO rather than silently
        // re-sending (avoids a duplicate) or leaving a Queued entry that
        // suppresses a fresh enqueue.
        self.pending_writes = doc
            .pending_writes
            .into_iter()
            .map(|mut p| {
                if p.status != PendingStatus::Parked {
                    p.status = PendingStatus::Parked;
                }
                p
            })
            .collect();
        for notice in doc.notices {
            self.notices.push_back(notice);
        }
        self.no_show_alerted = doc.no_show_alerted.into_iter().collect();
        self.world = recompute_world(self, now_millis);
    }

    /// Frees every station whose status names a bracket this config doesn't
    /// know (runs on every overlay path — adopted or re-keyed).
    fn reset_unknown_bracket_statuses(&mut self, now_millis: UnixMillis) {
        let stale: Vec<SetupId> = self
            .board
            .setups()
            .iter()
            .filter_map(|s| {
                let bracket = match &s.status {
                    SetupStatus::Called { bracket, .. } | SetupStatus::InProgress { bracket, .. } => Some(bracket),
                    SetupStatus::OccupiedExternal { set: Some((bracket, _)) } => Some(bracket),
                    SetupStatus::Free | SetupStatus::OccupiedExternal { set: None } => None,
                };
                bracket.filter(|b| self.bracket_ix(b).is_none()).map(|_| s.id)
            })
            .collect();
        if stale.is_empty() {
            return;
        }
        let numbers = stale.iter().map(|s| s.0.to_string()).collect::<Vec<_>>().join(", ");
        for id in stale {
            self.board.set_status(id, SetupStatus::Free);
        }
        let text = format!("freed setup(s) {numbers}: their persisted sets belong to a bracket outside this config");
        self.notice(now_millis, NoticeLevel::Warn, text);
    }

    /// Snapshots the last-good per-event set tables for the snapshot file
    /// (the offline cold-start seed).
    pub fn to_snapshot(&self) -> SnapshotDoc {
        SnapshotDoc {
            version: SNAPSHOT_VERSION,
            brackets: self
                .brackets
                .iter()
                .map(|b| BracketSnapshot {
                    id: b.state.id.clone(),
                    captured_at: b.last_good_poll.unwrap_or(0),
                    sets: b.state.sets.clone(),
                    groups: b.state.groups.clone(),
                })
                .collect(),
        }
    }

    /// Clones everything a background rollout evaluation needs (the simulator
    /// task borrows nothing from the Elm loop).
    pub fn sim_snapshot(&self, now_millis: UnixMillis) -> SimSnapshot {
        SimSnapshot {
            brackets: self.brackets.iter().map(|b| b.state.clone()).collect(),
            board: self.board.clone(),
            flags: self.flags.clone(),
            tombstones: self.tombstones.clone(),
            aliases: self.aliases.clone(),
            called_ints: self.called_ints.clone(),
            soft_busy: self.soft_busy.clone(),
            last_completed: self.last_completed.clone(),
            snoozes: self.snoozes.clone(),
            callable_since: self.callable_since.clone(),
            pool_overrides: self.pool_overrides.clone(),
            rest_window_secs: self.config.rest_window_secs,
            sim: self.config.sim.clone(),
            now_millis,
            durations: self.durations.clone(),
        }
    }

    /// All state ints we can interpret (deviation baseline).
    fn known_ints(&self) -> Vec<i32> {
        let mut known = self.config.known_benign_state_ints.clone();
        known.extend(&self.called_ints);
        known.extend(&self.in_progress_ints);
        known
    }

    fn bracket_ix(&self, id: &BracketId) -> Option<usize> {
        self.brackets.iter().position(|b| &b.state.id == id)
    }

    fn find_set(&self, bracket: &BracketId, key: &SetKey) -> Option<&LiveSet> {
        let ix = self.bracket_ix(bracket)?;
        self.brackets[ix].state.sets.iter().find(|s| &s.key == key)
    }
}

pub fn update(state: &mut AppState, msg: Msg, now_millis: UnixMillis) -> UpdateEffects {
    let mut effects = UpdateEffects::default();
    // Key/Poll/Write can all touch the persisted overlay; a bare Tick only
    // does when it fires a no-show alert (scan_no_shows marks it), so an idle
    // desk never churns saves. Rollout re-evaluations are asked for only when
    // the handler actually changed scheduling state (marked dirty) — pure
    // navigation must not burn a background sim — plus the explicit
    // decision-point requests handlers add themselves.
    match msg {
        Msg::Key(key) => {
            handle_key(state, key, now_millis, &mut effects);
            state.overlay_dirty = true;
            if state.dirty {
                effects.want_sim(SimUrgency::Routine);
            }
        }
        Msg::Poll(poll) => {
            handle_poll(state, poll, now_millis, &mut effects);
            state.overlay_dirty = true;
            if state.dirty {
                effects.want_sim(SimUrgency::Routine);
            }
        }
        Msg::Write(result) => {
            handle_write_result(state, result, now_millis);
            state.overlay_dirty = true;
            effects.want_sim(SimUrgency::Routine);
        }
        Msg::SimResult(rankings) => apply_sim_result(state, rankings),
        Msg::Tick => {
            scan_no_shows(state, now_millis);
            // A recompute runs a full forward sim; only refresh the
            // time-derived terms once they're meaningfully stale.
            if now_millis - state.last_recompute >= WORLD_REFRESH_MS {
                state.dirty = true;
            }
        }
    }
    if state.dirty {
        state.world = recompute_world(state, now_millis);
        state.ui.queue_ix = state.ui.queue_ix.min(state.world.queue.len().saturating_sub(1));
        state.dirty = false;
        state.last_recompute = now_millis;
    }
    effects
}

/// The one-refresh-per-modal-session policy: an open picker takes at most one
/// marked update ("ranking updated"); later results wait for the next
/// session. Everything else applies immediately.
fn apply_sim_result(state: &mut AppState, rankings: RolloutRankings) {
    match state.ui.modal.clone() {
        Some(Modal::CallPicker {
            setup,
            selected,
            refreshed,
        }) => {
            if refreshed {
                state.rollout_pending = Some(rankings);
            } else {
                state.rollout = Some(rankings);
                state.ui.modal = Some(Modal::CallPicker {
                    setup,
                    // The list may have reordered; clamp rather than chase.
                    selected: selected.min(picker_rows(state, setup).0.len().saturating_sub(1)),
                    refreshed: true,
                });
            }
        }
        _ => state.rollout = Some(rankings),
    }
}

// --- keys

fn handle_key(state: &mut AppState, key: KeyEvent, now: UnixMillis, effects: &mut UpdateEffects) {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        effects.quit = true;
        return;
    }
    if state.ui.modal.is_some() {
        handle_modal_key(state, key, now, effects);
        return;
    }
    match key.code {
        KeyCode::Char('q') => effects.quit = true,
        KeyCode::Char('?') => state.ui.modal = Some(Modal::Help),
        KeyCode::Char(c @ '0'..='9') => select_setup(state, c, now, effects),
        KeyCode::Char('p') => progress_selected(state, now, effects),
        KeyCode::Char('f') => free_selected(state, now, effects),
        KeyCode::Char('r') => requeue_selected(state, now, effects),
        KeyCode::Char('z') => snooze_selected(state, now),
        KeyCode::Char('u') => undo(state, now),
        KeyCode::Char('s') => state.ui.modal = Some(Modal::Setups { selected: 0 }),
        KeyCode::Char('i') => state.ui.modal = Some(Modal::Inspection { selected: 0 }),
        KeyCode::Char('n') => state.ui.modal = Some(Modal::Notices { selected: 0 }),
        KeyCode::Char('w') => state.ui.modal = Some(Modal::PendingWrites { selected: 0 }),
        KeyCode::Char('d') => open_flags_modal(state, now),
        KeyCode::Char('a') => open_reassign_modal(state, now),
        KeyCode::Char('g') => open_report_modal(state, now),
        KeyCode::Up => state.ui.queue_ix = state.ui.queue_ix.saturating_sub(1),
        KeyCode::Down => {
            state.ui.queue_ix = (state.ui.queue_ix + 1).min(state.world.queue.len().saturating_sub(1));
        }
        _ => {}
    }
}

fn handle_modal_key(state: &mut AppState, key: KeyEvent, now: UnixMillis, effects: &mut UpdateEffects) {
    if key.code == KeyCode::Esc {
        // The report modal steps back a stage instead of losing the draft.
        if let Some(Modal::Report(mut draft)) = state.ui.modal.take() {
            if draft.stage != ReportStage::Games {
                draft.stage = ReportStage::Games;
                state.ui.modal = Some(Modal::Report(draft));
                return;
            }
        }
        close_modal(state);
        return;
    }
    match state.ui.modal.clone() {
        Some(Modal::CallPicker {
            setup,
            selected,
            refreshed,
        }) => {
            let (rows, _) = picker_rows(state, setup);
            match key.code {
                KeyCode::Up => {
                    state.ui.modal = Some(Modal::CallPicker {
                        setup,
                        selected: selected.saturating_sub(1),
                        refreshed,
                    });
                }
                KeyCode::Down => {
                    state.ui.modal = Some(Modal::CallPicker {
                        setup,
                        selected: (selected + 1).min(rows.len().saturating_sub(1)),
                        refreshed,
                    });
                }
                KeyCode::Enter => commit_call(state, setup, selected, now, effects),
                // An exhausted pool presents an empty picker; `a` jumps
                // straight to reassignment for the same setup.
                KeyCode::Char('a') => state.ui.modal = Some(Modal::Reassign { setup, selected: 0 }),
                _ => {}
            }
        }
        Some(Modal::Inspection { selected }) => {
            let count = blocked_entries(state).len();
            match scroll(key.code, selected, count) {
                Some(next) => state.ui.modal = Some(Modal::Inspection { selected: next }),
                None => state.ui.modal = None,
            }
        }
        Some(Modal::Notices { selected }) => match key.code {
            KeyCode::Enter => ack_notice(state, selected),
            code => match scroll(code, selected, state.notices.len()) {
                Some(next) => state.ui.modal = Some(Modal::Notices { selected: next }),
                None => state.ui.modal = None,
            },
        },
        Some(Modal::PendingWrites { selected }) => match key.code {
            KeyCode::Enter => retry_parked(state, selected, now, effects),
            KeyCode::Char('d') => discard_pending(state, selected, now),
            code => match scroll(code, selected, state.pending_writes.len()) {
                Some(next) => state.ui.modal = Some(Modal::PendingWrites { selected: next }),
                None => state.ui.modal = None,
            },
        },
        Some(Modal::PlayerFlags { players, selected }) => match key.code {
            KeyCode::Enter => cycle_selected_flag(state, &players, selected, now),
            code => match scroll(code, selected, players.len()) {
                Some(next) => state.ui.modal = Some(Modal::PlayerFlags { players, selected: next }),
                None => state.ui.modal = None,
            },
        },
        Some(Modal::Reassign { setup, selected }) => match key.code {
            KeyCode::Enter => apply_reassign(state, setup, selected, now),
            code => match scroll(code, selected, reassign_options(state).len()) {
                Some(next) => state.ui.modal = Some(Modal::Reassign { setup, selected: next }),
                None => state.ui.modal = None,
            },
        },
        Some(Modal::Setups { selected }) => match key.code {
            KeyCode::Enter => apply_setups_row(state, selected, now, effects),
            code => match scroll(code, selected, setups_rows(state).len()) {
                Some(next) => state.ui.modal = Some(Modal::Setups { selected: next }),
                None => state.ui.modal = None,
            },
        },
        Some(Modal::Report(draft)) => handle_report_key(state, *draft, key.code, now, effects),
        // Help modal: any other key closes it too.
        Some(Modal::Help) | None => state.ui.modal = None,
    }
}

/// Shared list-modal cursor: Up/Down move (clamped), anything else closes.
fn scroll(code: KeyCode, selected: usize, count: usize) -> Option<usize> {
    match code {
        KeyCode::Up => Some(selected.saturating_sub(1)),
        KeyCode::Down => Some((selected + 1).min(count.saturating_sub(1))),
        _ => None,
    }
}

/// Closing any modal ends the picker's modal session: a rollout result held
/// back by the one-refresh policy applies now.
fn close_modal(state: &mut AppState) {
    state.ui.modal = None;
    if let Some(pending) = state.rollout_pending.take() {
        state.rollout = Some(pending);
    }
}

/// The rows the call-picker shows and commits against: the rollout ranking
/// when one is available for the setup, else the greedy world ranking.
/// Returns `(rows, from_rollout)`.
pub(crate) fn picker_rows(state: &AppState, setup: SetupId) -> (Vec<RolloutRow>, bool) {
    if let Some(rows) = state.rollout.as_ref().and_then(|r| r.per_setup.get(&setup)) {
        if !rows.is_empty() {
            return (rows.clone(), true);
        }
    }
    let greedy = state
        .world
        .per_setup
        .get(&setup)
        .map(|entries| entries.iter().cloned().map(|e| RolloutRow::Call(Box::new(e))).collect())
        .unwrap_or_default();
    (greedy, false)
}

/// Digits map straight to the TO's setup numbering (`SetupId(d)`, `0` = 10).
fn select_setup(state: &mut AppState, digit: char, now: UnixMillis, effects: &mut UpdateEffects) {
    let number = digit.to_digit(10).map(|d| if d == 0 { 10 } else { d }).unwrap_or(0);
    let setup = SetupId(number);
    let Some(status) = state.board.setups().iter().find(|s| s.id == setup).map(|s| s.status.clone()) else {
        state.notice(now, NoticeLevel::Warn, format!("no setup {number} configured"));
        return;
    };
    match status {
        SetupStatus::Free => {
            state.ui.selected_setup = Some(setup);
            // A fresh modal session starts on the freshest ranking.
            if let Some(pending) = state.rollout_pending.take() {
                state.rollout = Some(pending);
            }
            state.ui.modal = Some(Modal::CallPicker {
                setup,
                selected: 0,
                refreshed: false,
            });
            // Opening the picker is a decision point; ask for a fresh rollout
            // (navigation keys alone no longer schedule one).
            effects.want_sim(SimUrgency::Routine);
        }
        _ => {
            state.ui.selected_setup = Some(setup);
        }
    }
}

fn commit_call(state: &mut AppState, setup: SetupId, selected: usize, now: UnixMillis, effects: &mut UpdateEffects) {
    let (rows, _) = picker_rows(state, setup);
    close_modal(state);
    let entry = match rows.get(selected) {
        Some(RolloutRow::Call(entry)) => (**entry).clone(),
        Some(RolloutRow::Hold { .. }) => {
            state.notice(now, NoticeLevel::Info, format!("holding setup {} open", setup.0));
            return;
        }
        None => {
            state.notice(now, NoticeLevel::Warn, "no candidate selected");
            return;
        }
    };

    // Re-verify against *current* state: the world snapshot may predate a
    // poll or another local action.
    if assigned_sets(&state.board).contains(&(entry.bracket.clone(), entry.key.clone())) {
        state.notice(now, NoticeLevel::Warn, format!("{} is already on a setup", entry.players));
        return;
    }
    match verify_callable(state, &entry.bracket, &entry.key, setup, now) {
        Ok(_) => {}
        Err(reasons) => {
            state.notice(
                now,
                NoticeLevel::Warn,
                format!("{} is no longer callable: {reasons:?}", entry.players),
            );
            state.dirty = true;
            return;
        }
    }

    push_undo(state, format!("call {} on setup {}", entry.players, setup.0));
    state.board.set_status(
        setup,
        SetupStatus::Called {
            bracket: entry.bracket.clone(),
            set: entry.key.clone(),
        },
    );
    state.called_at.insert((entry.bracket.clone(), entry.key.clone()), now);
    state.ui.selected_setup = Some(setup);
    state.dirty = true;

    enqueue_write(state, effects, &entry.bracket, &entry.key, &entry.id.0, WriteKind::Called, now);
    let text = format!("called {} ({}) on setup {}", entry.players, entry.round_text, setup.0);
    state.notice(now, NoticeLevel::Info, text);
}

/// The status the hot keys act on, with its set identity.
fn selected_assignment(state: &AppState) -> Option<(SetupId, SetupStatus)> {
    let setup = state.ui.selected_setup?;
    let status = state.board.setups().iter().find(|s| s.id == setup)?.status.clone();
    Some((setup, status))
}

fn progress_selected(state: &mut AppState, now: UnixMillis, effects: &mut UpdateEffects) {
    let Some((setup, SetupStatus::Called { bracket, set })) = selected_assignment(state) else {
        state.notice(now, NoticeLevel::Warn, "select a Called setup first (digit), then p");
        return;
    };
    push_undo(state, format!("mark setup {} in progress", setup.0));
    state.board.set_status(
        setup,
        SetupStatus::InProgress {
            bracket: bracket.clone(),
            set: set.clone(),
        },
    );
    state.dirty = true;
    let id = state.find_set(&bracket, &set).map(|s| s.id.0.clone()).unwrap_or_default();
    enqueue_write(state, effects, &bracket, &set, &id, WriteKind::InProgress, now);
}

fn free_selected(state: &mut AppState, now: UnixMillis, effects: &mut UpdateEffects) {
    let Some((setup, status)) = selected_assignment(state) else {
        state.notice(now, NoticeLevel::Warn, "select a setup first (digit), then f");
        return;
    };
    let (SetupStatus::Called { bracket, set } | SetupStatus::InProgress { bracket, set }) = status else {
        state.notice(now, NoticeLevel::Warn, format!("setup {} holds no set to free", setup.0));
        return;
    };
    push_undo(state, format!("free setup {}", setup.0));
    state.board.set_status(setup, SetupStatus::Free);
    // We believe the match finished; suppress our own stale evidence until
    // the server confirms, and poll that event now.
    state.tombstones.awaiting_remote_completion.insert((bracket.clone(), set.clone()));
    state.no_show_alerted.remove(&(bracket.clone(), set.clone()));
    effects.force_poll.push(bracket.clone());
    // Decision-point exemption: evaluate the post-free world immediately.
    effects.want_sim(SimUrgency::Immediate);
    state.dirty = true;
    state.notice(now, NoticeLevel::Info, format!("setup {} freed, awaiting result", setup.0));
}

fn requeue_selected(state: &mut AppState, now: UnixMillis, effects: &mut UpdateEffects) {
    let Some((setup, status)) = selected_assignment(state) else {
        state.notice(now, NoticeLevel::Warn, "select a setup first (digit), then r");
        return;
    };
    let (SetupStatus::Called { bracket, set } | SetupStatus::InProgress { bracket, set }) = status else {
        state.notice(now, NoticeLevel::Warn, format!("setup {} holds no set to re-queue", setup.0));
        return;
    };
    push_undo(state, format!("re-queue setup {}", setup.0));
    state.board.set_status(setup, SetupStatus::Free);
    let pair = (bracket.clone(), set.clone());
    // Un-call: our own mutations may already have stamped CALLED state and
    // startedAt remotely; suppress both so the set ranks again.
    state.tombstones.suppress_remote_called.insert(pair.clone());
    state.tombstones.suppress_remote_active.insert(pair.clone());
    state.called_at.remove(&pair);
    state.no_show_alerted.remove(&pair);
    effects.want_sim(SimUrgency::Immediate);
    state.dirty = true;
    state.notice(now, NoticeLevel::Info, format!("setup {} re-queued its set", setup.0));
}

fn snooze_selected(state: &mut AppState, now: UnixMillis) {
    let Some(entry) = state.world.queue.get(state.ui.queue_ix).cloned() else {
        return;
    };
    push_undo(state, format!("snooze {}", entry.players));
    state
        .snoozes
        .insert((entry.bracket.clone(), entry.key.clone()), now + SNOOZE_SECS * 1000);
    state.dirty = true;
    state.notice(
        now,
        NoticeLevel::Info,
        format!("snoozed {} for {}m", entry.players, SNOOZE_SECS / 60),
    );
}

fn undo(state: &mut AppState, now: UnixMillis) {
    let Some(snap) = state.undo.take() else {
        state.notice(now, NoticeLevel::Warn, "nothing to undo");
        return;
    };
    state.board = snap.board;
    state.tombstones = snap.tombstones;
    state.flags = snap.flags;
    state.pool_overrides = snap.pool_overrides;
    state.snoozes = snap.snoozes;
    state.called_at = snap.called_at;
    state.dirty = true;
    state.notice(now, NoticeLevel::Info, format!("undid: {}", snap.description));
}

fn push_undo(state: &mut AppState, description: String) {
    state.undo = Some(UndoSnapshot {
        board: state.board.clone(),
        tombstones: state.tombstones.clone(),
        flags: state.flags.clone(),
        pool_overrides: state.pool_overrides.clone(),
        snoozes: state.snoozes.clone(),
        called_at: state.called_at.clone(),
        description,
    });
}

/// Queues a mutation intent when the set id is numeric and writes are armed.
/// One pending intent per (set, kind): re-keys never double-queue, while a
/// Called and an InProgress for the same set may coexist (the writer
/// serializes per set).
fn enqueue_write(
    state: &mut AppState,
    effects: &mut UpdateEffects,
    bracket: &BracketId,
    key: &SetKey,
    raw_id: &str,
    kind: WriteKind,
    now: UnixMillis,
) {
    let Ok(id) = raw_id.parse::<StartGgId>() else {
        state.notice(
            now,
            NoticeLevel::Warn,
            "set has a preview id (bracket not started); recorded locally only",
        );
        return;
    };
    if !state.writes_armed {
        return;
    }
    // Queued or reconnect-held both count as in flight for single-flight.
    // Kinds compare by discriminant: a second Report for the same set is a
    // duplicate even when its payload differs.
    if state
        .pending_writes
        .iter()
        .any(|p| p.intent.id == id && discriminant(&p.intent.kind) == discriminant(&kind) && p.status != PendingStatus::Parked)
    {
        return;
    }
    let intent = WriteIntent {
        bracket: bracket.clone(),
        key: key.clone(),
        id,
        kind,
        created_at: now,
    };
    state.pending_writes.push(PendingWrite {
        intent: intent.clone(),
        status: PendingStatus::Queued,
        attempts: 0,
        last_error: None,
    });
    effects.writes.push(intent);
}

/// The inspection view's row set: every blocked (bracket, set) pair in a
/// deterministic order. Rendering and cursor bounds share this.
pub(crate) fn blocked_entries(state: &AppState) -> Vec<(BracketId, SetKey)> {
    let mut keys: Vec<(BracketId, SetKey)> = state.world.blocked.keys().cloned().collect();
    keys.sort();
    keys
}

/// Acks the notice at `display_ix` in newest-first order (the notices page's
/// presentation order).
fn ack_notice(state: &mut AppState, display_ix: usize) {
    let len = state.notices.len();
    if display_ix >= len {
        return;
    }
    if let Some(notice) = state.notices.get_mut(len - 1 - display_ix) {
        notice.acked = true;
    }
}

/// Enter on a parked write: re-queue it with a fresh attempt budget.
fn retry_parked(state: &mut AppState, selected: usize, now: UnixMillis, effects: &mut UpdateEffects) {
    let Some(pending) = state.pending_writes.get_mut(selected) else {
        return;
    };
    if pending.status != PendingStatus::Parked {
        return;
    }
    pending.status = PendingStatus::Queued;
    pending.attempts = 0;
    let intent = pending.intent.clone();
    state.notice(
        now,
        NoticeLevel::Info,
        format!("retrying write {} for set {}", intent.kind.label(), intent.id),
    );
    effects.writes.push(intent);
}

/// `d` on a parked write: drop it for good (the TO handled it site-side).
fn discard_pending(state: &mut AppState, selected: usize, now: UnixMillis) {
    let Some(pending) = state.pending_writes.get(selected) else {
        return;
    };
    if pending.status != PendingStatus::Parked {
        return;
    }
    let intent = pending.intent.clone();
    state.pending_writes.remove(selected);
    state.held_writes.retain(|i| *i != intent);
    state.notice(
        now,
        NoticeLevel::Info,
        format!("discarded write {} for set {}", intent.kind.label(), intent.id),
    );
}

/// `d` on the main view: tri-state flags for the highlighted queue entry's
/// players.
fn open_flags_modal(state: &mut AppState, now: UnixMillis) {
    let Some(entry) = state.world.queue.get(state.ui.queue_ix) else {
        state.notice(now, NoticeLevel::Warn, "highlight a queue entry first (Up/Down), then d");
        return;
    };
    let Some(set) = state.find_set(&entry.bracket, &entry.key) else {
        return;
    };
    let players: Vec<(ConflictKey, String)> = set
        .occupants()
        .flat_map(|o| {
            let name = o.display_name.clone();
            occupant_keys(o, &state.aliases).into_iter().map(move |k| (k, name.clone()))
        })
        .collect();
    if players.is_empty() {
        return;
    }
    state.ui.modal = Some(Modal::PlayerFlags { players, selected: 0 });
}

/// `a` on the main view: reassign the selected setup's pool.
fn open_reassign_modal(state: &mut AppState, now: UnixMillis) {
    let Some(setup) = state.ui.selected_setup else {
        state.notice(now, NoticeLevel::Warn, "select a setup first (digit), then a");
        return;
    };
    state.ui.modal = Some(Modal::Reassign { setup, selected: 0 });
}

fn apply_reassign(state: &mut AppState, setup: SetupId, selected: usize, now: UnixMillis) {
    let options = reassign_options(state);
    let Some(option) = options.get(selected) else {
        return;
    };
    push_undo(state, format!("reassign setup {}", setup.0));
    let text = match option {
        ReassignOption::Dedicate(bracket) => {
            state.pool_overrides.insert(setup, PoolOverride::Dedicated(bracket.clone()));
            format!("setup {} now takes only {}", setup.0, bracket.0)
        }
        ReassignOption::AllowAny => {
            state.pool_overrides.insert(setup, PoolOverride::AllowAny);
            format!("setup {} now open to every bracket", setup.0)
        }
        ReassignOption::RestoreConfig => {
            state.pool_overrides.remove(&setup);
            format!("setup {} restored to its config pools", setup.0)
        }
    };
    state.ui.modal = None;
    state.dirty = true;
    state.notice(now, NoticeLevel::Info, text);
}

/// Enter in the setups modal: retire the selected station or add one of the
/// selected type. The modal stays open (batch edits); the cursor clamps to
/// the rebuilt row list.
fn apply_setups_row(state: &mut AppState, selected: usize, now: UnixMillis, effects: &mut UpdateEffects) {
    match setups_rows(state).get(selected) {
        Some(SetupsRow::Retire(id, _)) => retire_setup(state, *id, now, effects),
        Some(SetupsRow::Add(setup_type)) => add_setup_station(state, setup_type.clone(), now, effects),
        None => return,
    }
    let len = setups_rows(state).len();
    state.ui.modal = Some(Modal::Setups {
        selected: selected.min(len.saturating_sub(1)),
    });
}

fn retire_setup(state: &mut AppState, id: SetupId, now: UnixMillis, effects: &mut UpdateEffects) {
    let Some(setup) = state.board.setups().iter().find(|s| s.id == id) else {
        return;
    };
    if setup.status != SetupStatus::Free {
        state.notice(now, NoticeLevel::Warn, format!("setup {} is occupied — free it first (f/r)", id.0));
        return;
    }
    push_undo(state, format!("retire setup {}", id.0));
    state.board.remove_setup(id);
    // A stale Dedicated override must not re-attach when the next arrival
    // reuses this number.
    state.pool_overrides.remove(&id);
    if state.ui.selected_setup == Some(id) {
        state.ui.selected_setup = None;
    }
    state.dirty = true;
    // Close the stale-rollout window: the next picker must not offer a
    // station that no longer exists.
    effects.want_sim(SimUrgency::Immediate);
    state.notice(now, NoticeLevel::Info, format!("retired setup {}", id.0));
}

fn add_setup_station(state: &mut AppState, setup_type: String, now: UnixMillis, effects: &mut UpdateEffects) {
    let id = state.board.lowest_unused_id();
    push_undo(state, format!("add setup {}", id.0));
    state.board.add_setup(id, setup_type.clone());
    if id.0 > 10 {
        let text = format!(
            "setup {} is beyond digit selection (1-9, 0 = 10) — free/retire a lower number to use it",
            id.0
        );
        state.notice(now, NoticeLevel::Warn, text);
    }
    state.dirty = true;
    effects.want_sim(SimUrgency::Immediate);
    state.notice(now, NoticeLevel::Info, format!("added setup {} ({setup_type})", id.0));
}

/// `g` on the main view: report the selected setup's set, game by game.
fn open_report_modal(state: &mut AppState, now: UnixMillis) {
    if !state.writes_armed {
        state.notice(now, NoticeLevel::Warn, "reporting needs writes armed (advisor-only session)");
        return;
    }
    let Some((setup, status)) = selected_assignment(state) else {
        state.notice(now, NoticeLevel::Warn, "select a setup first (digit), then g");
        return;
    };
    let (SetupStatus::Called { bracket, set } | SetupStatus::InProgress { bracket, set }) = status else {
        state.notice(now, NoticeLevel::Warn, format!("setup {} holds no set to report", setup.0));
        return;
    };

    let draft = {
        let Some(live) = state.find_set(&bracket, &set) else {
            state.notice(now, NoticeLevel::Warn, "the set is gone from the local table");
            return;
        };
        if live.id.0.parse::<u64>().is_err() {
            state.notice(
                now,
                NoticeLevel::Warn,
                "can't report: set still has a preview id (bracket not started)",
            );
            return;
        }
        let mut sides = live.slots.iter().filter_map(|slot| slot.occupant.as_ref()).map(|o| ReportSide {
            entrant_id: o.entrant_id.0.clone(),
            name: o.display_name.clone(),
            sticky_key: o.player_ids.first().map(|p| p.0.clone()).unwrap_or_else(|| o.entrant_id.0.clone()),
        });
        let (Some(left), Some(right)) = (sides.next(), sides.next()) else {
            state.notice(now, NoticeLevel::Warn, "the set doesn't have two entrants to report");
            return;
        };
        let best_of = state
            .bracket_ix(&bracket)
            .and_then(|ix| state.brackets[ix].state.groups.iter().find(|g| g.id == set.phase_group))
            .and_then(|g| g.best_of_by_round.get(&set.round).copied());
        let chars = [
            state.last_characters.get(&left.sticky_key).copied(),
            state.last_characters.get(&right.sticky_key).copied(),
        ];
        ReportDraft {
            setup,
            bracket,
            key: set,
            raw_id: live.id.0.clone(),
            left,
            right,
            best_of,
            games: Vec::new(),
            chars,
            game_cursor: 0,
            stage: ReportStage::Games,
        }
    };
    state.ui.modal = Some(Modal::Report(Box::new(draft)));
}

fn handle_report_key(state: &mut AppState, mut draft: ReportDraft, code: KeyCode, now: UnixMillis, effects: &mut UpdateEffects) {
    match draft.stage.clone() {
        ReportStage::Games => match code {
            KeyCode::Char('1') => record_game(state, draft, Side::Left),
            KeyCode::Char('2') => record_game(state, draft, Side::Right),
            KeyCode::Backspace => {
                draft.games.pop();
                draft.game_cursor = draft.game_cursor.min(draft.games.len().saturating_sub(1));
                state.ui.modal = Some(Modal::Report(Box::new(draft)));
            }
            KeyCode::Up => {
                draft.game_cursor = draft.game_cursor.saturating_sub(1);
                state.ui.modal = Some(Modal::Report(Box::new(draft)));
            }
            KeyCode::Down => {
                draft.game_cursor = (draft.game_cursor + 1).min(draft.games.len().saturating_sub(1));
                state.ui.modal = Some(Modal::Report(Box::new(draft)));
            }
            KeyCode::Char('c') => {
                if report_roster(state, &draft.bracket).is_empty() {
                    state.notice(now, NoticeLevel::Warn, "no character data for this event");
                } else {
                    draft.stage = ReportStage::Characters {
                        side: Side::Left,
                        filter: String::new(),
                        cursor: 0,
                    };
                }
                state.ui.modal = Some(Modal::Report(Box::new(draft)));
            }
            KeyCode::Char('d') => {
                draft.stage = ReportStage::DqPick;
                state.ui.modal = Some(Modal::Report(Box::new(draft)));
            }
            KeyCode::Enter => {
                if draft.games.is_empty() {
                    state.notice(now, NoticeLevel::Warn, "record game winners first (1/2)");
                } else if draft.leader().is_none() {
                    state.notice(now, NoticeLevel::Warn, "score is tied — record the decider first");
                } else {
                    draft.stage = ReportStage::Confirm { dq: None };
                }
                state.ui.modal = Some(Modal::Report(Box::new(draft)));
            }
            _ => state.ui.modal = Some(Modal::Report(Box::new(draft))),
        },
        ReportStage::Characters { side, filter, cursor } => {
            handle_character_key(state, draft, side, filter, cursor, code);
        }
        ReportStage::DqPick => {
            match code {
                KeyCode::Char('1') => draft.stage = ReportStage::Confirm { dq: Some(Side::Left) },
                KeyCode::Char('2') => draft.stage = ReportStage::Confirm { dq: Some(Side::Right) },
                _ => {}
            }
            state.ui.modal = Some(Modal::Report(Box::new(draft)));
        }
        ReportStage::Confirm { dq } => match code {
            KeyCode::Enter | KeyCode::Char('y') => submit_report(state, draft, dq, now, effects),
            _ => state.ui.modal = Some(Modal::Report(Box::new(draft))),
        },
    }
}

fn record_game(state: &mut AppState, mut draft: ReportDraft, winner: Side) {
    // A new game assumes the previous game's characters (the TO edits the
    // exceptions).
    let chars = draft.games.last().map_or(draft.chars, |g| g.chars);
    draft.games.push(GameDraft { winner, chars });
    draft.game_cursor = draft.games.len() - 1;
    // A clinched best-of needs no further entry; jump to the summary.
    if draft.clinched() {
        draft.stage = ReportStage::Confirm { dq: None };
    }
    state.ui.modal = Some(Modal::Report(Box::new(draft)));
}

fn handle_character_key(state: &mut AppState, mut draft: ReportDraft, side: Side, mut filter: String, cursor: usize, code: KeyCode) {
    let matches_len = filtered_roster(report_roster(state, &draft.bracket), &filter).len();
    match code {
        KeyCode::Enter => {
            let choice = filtered_roster(report_roster(state, &draft.bracket), &filter)
                .get(cursor)
                .map(|c| c.id);
            if let Some(id) = choice {
                // Apply from the targeted game onward — carry-forward means
                // a switch mid-set holds for the rest of it.
                draft.chars[side.ix()] = Some(id);
                for game in draft.games.iter_mut().skip(draft.game_cursor) {
                    game.chars[side.ix()] = Some(id);
                }
                state.last_characters.insert(draft.side(side).sticky_key.clone(), id);
            }
            advance_character_stage(&mut draft, side);
        }
        // Tab keeps whatever the side already had (sticky or nothing).
        KeyCode::Tab => advance_character_stage(&mut draft, side),
        KeyCode::Up => {
            draft.stage = ReportStage::Characters {
                side,
                filter,
                cursor: cursor.saturating_sub(1),
            };
        }
        KeyCode::Down => {
            draft.stage = ReportStage::Characters {
                side,
                filter,
                cursor: (cursor + 1).min(matches_len.saturating_sub(1)),
            };
        }
        KeyCode::Backspace => {
            filter.pop();
            draft.stage = ReportStage::Characters { side, filter, cursor: 0 };
        }
        KeyCode::Char(c) if c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '.' | '&') => {
            filter.push(c);
            draft.stage = ReportStage::Characters { side, filter, cursor: 0 };
        }
        _ => {}
    }
    state.ui.modal = Some(Modal::Report(Box::new(draft)));
}

/// Left picks first, then right, then back to the game taps.
fn advance_character_stage(draft: &mut ReportDraft, side: Side) {
    draft.stage = match side {
        Side::Left => ReportStage::Characters {
            side: Side::Right,
            filter: String::new(),
            cursor: 0,
        },
        Side::Right => ReportStage::Games,
    };
}

/// The roster the report modal picks characters from.
pub(crate) fn report_roster<'a>(state: &'a AppState, bracket: &BracketId) -> &'a [CharacterInfo] {
    state
        .brackets
        .iter()
        .find(|b| &b.state.id == bracket)
        .map(|b| b.characters.as_slice())
        .unwrap_or(&[])
}

/// Case-insensitive roster filter, prefix matches first.
pub(crate) fn filtered_roster<'a>(roster: &'a [CharacterInfo], filter: &str) -> Vec<&'a CharacterInfo> {
    let needle = filter.to_lowercase();
    let (mut prefix, mut rest): (Vec<&CharacterInfo>, Vec<&CharacterInfo>) = roster
        .iter()
        .filter(|c| c.name.to_lowercase().contains(&needle))
        .partition(|c| c.name.to_lowercase().starts_with(&needle));
    prefix.append(&mut rest);
    prefix
}

fn submit_report(state: &mut AppState, mut draft: ReportDraft, dq: Option<Side>, now: UnixMillis, effects: &mut UpdateEffects) {
    let winner_side = match dq {
        Some(dq_side) => dq_side.other(),
        None => match draft.leader() {
            Some(side) => side,
            None => {
                state.notice(now, NoticeLevel::Warn, "score is tied — record the decider first");
                draft.stage = ReportStage::Games;
                state.ui.modal = Some(Modal::Report(Box::new(draft)));
                return;
            }
        },
    };
    let summary = draft.summary(dq);

    // DQs report winner-only (no game data), matching the web flow.
    let games: Vec<GameReport> = if dq.is_some() {
        Vec::new()
    } else {
        draft
            .games
            .iter()
            .map(|game| GameReport {
                winner_entrant_id: Some(draft.side(game.winner).entrant_id.clone()),
                selections: game_selections(&draft, game),
            })
            .collect()
    };

    push_undo(state, format!("report {summary}"));
    let pair = (draft.bracket.clone(), draft.key.clone());
    // Same shape as `f`: we believe the match is over — free the station,
    // suppress our own stale evidence, and poll for the confirmed result.
    let still_holds = state.board.setups().iter().any(|s| {
        s.id == draft.setup
            && match &s.status {
                SetupStatus::Called { bracket, set } | SetupStatus::InProgress { bracket, set } => (bracket, set) == (&pair.0, &pair.1),
                _ => false,
            }
    });
    if still_holds {
        state.board.set_status(draft.setup, SetupStatus::Free);
    }
    state.tombstones.awaiting_remote_completion.insert(pair.clone());
    state.called_at.remove(&pair);
    state.no_show_alerted.remove(&pair);
    effects.force_poll.push(draft.bracket.clone());
    effects.want_sim(SimUrgency::Immediate);

    let payload = ReportPayload {
        winner_entrant_id: Some(draft.side(winner_side).entrant_id.clone()),
        is_dq: dq.is_some(),
        games,
        summary: summary.clone(),
    };
    enqueue_write(
        state,
        effects,
        &draft.bracket,
        &draft.key,
        &draft.raw_id,
        WriteKind::Report(Box::new(payload)),
        now,
    );
    close_modal(state);
    state.dirty = true;
    state.notice(now, NoticeLevel::Info, format!("reported {summary}; setup {} freed", draft.setup.0));
}

/// One game's character picks per side (sides without one stay out).
fn game_selections(draft: &ReportDraft, game: &GameDraft) -> Vec<GameSelection> {
    [Side::Left, Side::Right]
        .into_iter()
        .filter_map(|side| {
            Some(GameSelection {
                entrant_id: draft.side(side).entrant_id.clone(),
                character_id: Some(game.chars[side.ix()]?),
            })
        })
        .collect()
}

fn cycle_selected_flag(state: &mut AppState, players: &[(ConflictKey, String)], selected: usize, now: UnixMillis) {
    let Some((key, name)) = players.get(selected) else {
        return;
    };
    push_undo(state, format!("flag change for {name}"));
    let label = cycle_flag(&mut state.flags, key);
    state.dirty = true;
    state.notice(now, NoticeLevel::Info, format!("{name}: {label}"));
}

/// resting → departed → force-available → clear. Returns the new state's
/// label for the notice.
fn cycle_flag(flags: &mut PlayerFlags, key: &ConflictKey) -> &'static str {
    if flags.resting.remove(key) {
        flags.departed.insert(key.clone());
        "departed"
    } else if flags.departed.remove(key) {
        flags.force_available.insert(key.clone());
        "force-available"
    } else if flags.force_available.remove(key) {
        "flags cleared"
    } else {
        flags.resting.insert(key.clone());
        "resting"
    }
}

/// The flag a key currently carries, for display.
pub(crate) fn flag_label(flags: &PlayerFlags, key: &ConflictKey) -> &'static str {
    if flags.resting.contains(key) {
        "resting"
    } else if flags.departed.contains(key) {
        "departed"
    } else if flags.force_available.contains(key) {
        "force-available"
    } else {
        "—"
    }
}

/// Re-runs the real conflict predicate for one set against current state.
fn verify_callable(
    state: &AppState,
    bracket: &BracketId,
    key: &SetKey,
    setup: SetupId,
    now: UnixMillis,
) -> Result<CallableSet, Vec<BlockReason>> {
    let Some(ix) = state.bracket_ix(bracket) else {
        return Err(vec![]);
    };
    let Some(set) = state.brackets[ix].state.sets.iter().find(|s| &s.key == key) else {
        return Err(vec![]);
    };

    let inputs = ConflictInputs {
        aliases: &state.aliases,
        board: &state.board,
        flags: &state.flags,
        tombstones: &state.tombstones,
        called_ints: &state.called_ints,
        soft_busy: &state.soft_busy,
        last_completed: &state.last_completed,
        rest_window_secs: state.config.rest_window_secs,
        snoozes: &state.snoozes,
    };
    let pools: Vec<Vec<SetupId>> = state
        .brackets
        .iter()
        .map(|b| effective_pool(&b.state.id, &b.state.setup_types, state.board.setups(), &state.pool_overrides))
        .collect();
    let views: Vec<BracketView<'_>> = state
        .brackets
        .iter()
        .zip(&pools)
        .map(|(b, pool)| BracketView {
            id: &b.state.id,
            sets: &b.state.sets,
            mode: b.state.mode,
            start_at: b.state.start_at,
            held: b.state.held,
            pool,
        })
        .collect();
    let index = ConflictIndex::build(&views, &inputs);
    let view = &views[ix];
    callable(view, set, &index, &inputs, now).and_then(|c| {
        if c.candidate_setups.contains(&setup) {
            Ok(c)
        } else {
            Err(vec![BlockReason::NoPermittedFreeSetup])
        }
    })
}

// --- polls

fn handle_poll(state: &mut AppState, poll: PollResult, now: UnixMillis, effects: &mut UpdateEffects) {
    let Some(ix) = state.bracket_ix(&poll.bracket) else {
        state.notice(now, NoticeLevel::Warn, format!("poll for unknown bracket {}", poll.bracket.0));
        return;
    };
    match poll.outcome {
        PollOutcome::Failed(failure) => {
            let runtime = &mut state.brackets[ix];
            runtime.consecutive_failures += 1;
            let health = match &failure {
                PollFailure::Offline => PollHealth::Offline,
                PollFailure::Transient => PollHealth::Transient,
                PollFailure::Persistent(e) => PollHealth::Persistent(e.clone()),
            };
            let escalated = matches!(health, PollHealth::Persistent(_)) && runtime.health != health;
            runtime.health = health;
            if escalated {
                let text = format!("polling {} failing persistently: {failure:?}", poll.bracket.0);
                state.notice(now, NoticeLevel::Error, text);
            }
        }
        PollOutcome::Snapshot { sets, warnings, skipped } => {
            if poll.seq <= state.brackets[ix].applied_seq {
                return;
            }
            apply_snapshot(state, ix, poll.seq, poll.captured_at, sets, now, effects);
            if !skipped.is_empty() {
                let text = format!("{}: {} sets skipped in conversion", poll.bracket.0, skipped.len());
                state.notice(now, NoticeLevel::Error, text);
            }
            // Warnings (identity-degraded etc.) recur every poll; surface
            // only new unknown-vocabulary warnings to keep the ring useful.
            for warning in warnings {
                if let ModelWarning::UnknownPrereqType { set, raw } = warning {
                    let text = format!("unknown prereqType {raw:?} on {set:?}");
                    state.notice(now, NoticeLevel::Warn, text);
                }
            }
        }
    }
}

fn apply_snapshot(
    state: &mut AppState,
    ix: usize,
    seq: u64,
    captured_at: UnixMillis,
    mut sets: Vec<LiveSet>,
    now: UnixMillis,
    effects: &mut UpdateEffects,
) {
    let bracket_id = state.brackets[ix].state.id.clone();
    let dropped = apply_tearing_guard(&mut state.brackets[ix], &mut sets);
    if dropped > 0 {
        let text = format!("{}: {dropped} set(s) removed server-side (absent two polls)", bracket_id.0);
        state.notice(now, NoticeLevel::Warn, text);
    }
    let prev = std::mem::replace(&mut state.brackets[ix].state.sets, sets);
    {
        let runtime = &mut state.brackets[ix];
        runtime.applied_seq = seq;
        runtime.last_good_poll = Some(captured_at);
        runtime.consecutive_failures = 0;
        runtime.health = PollHealth::Ok;
    }

    let diff = diff_snapshots(&prev, &state.brackets[ix].state.sets, &state.aliases);

    for completed in &diff.completed {
        let best_of = state.brackets[ix]
            .state
            .groups
            .iter()
            .find(|g| g.id == completed.key.phase_group)
            .and_then(|g| g.best_of_by_round.get(&completed.key.round).copied());
        let called_at = state.called_at.get(&(bracket_id.clone(), completed.key.clone())).copied();
        let offset_secs = state.clock_offset.map_or(0, |s| s.offset_secs);
        state.durations.ingest(&bracket_id, completed, best_of, called_at, offset_secs);
    }
    for (key, at) in &diff.last_completed {
        let entry = state.last_completed.entry(key.clone()).or_insert(*at);
        *entry = (*entry).max(*at);
    }
    for key in &diff.results_arrived {
        let pair = (bracket_id.clone(), key.clone());
        state.tombstones.awaiting_remote_completion.remove(&pair);
        state.tombstones.suppress_remote_active.remove(&pair);
        state.tombstones.suppress_remote_called.remove(&pair);
        state.callable_since.remove(key);
        state.snoozes.remove(&pair);
        state.called_at.remove(&pair);
        state.no_show_alerted.remove(&pair);
        if free_setups_holding(state, &pair, now) > 0 {
            effects.want_sim(SimUrgency::Immediate);
        }
    }

    ingest_deviations(state, ix, &prev, now);
    stamp_ready(state, ix, now);
    release_held_writes(state, ix, captured_at, now, effects);
    state.dirty = true;
    state.snapshot_dirty = true;
}

/// The flush discipline's release half: a successful poll for an event
/// revalidates that event's reconnect-held intents against the fresh
/// snapshot — moot targets (vanished, already completed) drop with a notice,
/// live ones re-queue to the writer. The strictly-newer guard means an intent
/// created after this snapshot was captured keeps waiting.
fn release_held_writes(state: &mut AppState, ix: usize, captured_at: UnixMillis, now: UnixMillis, effects: &mut UpdateEffects) {
    let bracket_id = state.brackets[ix].state.id.clone();
    let (candidates, still_held): (Vec<WriteIntent>, Vec<WriteIntent>) = std::mem::take(&mut state.held_writes)
        .into_iter()
        .partition(|i| i.bracket == bracket_id && captured_at > i.created_at);
    state.held_writes = still_held;

    for intent in candidates {
        let target = state.brackets[ix].state.sets.iter().find(|s| s.key == intent.key);
        let moot = match target {
            None => Some("its set vanished"),
            Some(set) if set.is_completed() => Some("the set already completed"),
            Some(_) => None,
        };
        if let Some(reason) = moot {
            state.pending_writes.retain(|p| p.intent != intent);
            let text = format!("dropped held write {} for set {}: {reason}", intent.kind.label(), intent.id);
            state.notice(now, NoticeLevel::Warn, text);
            continue;
        }
        if let Some(pending) = state.pending_writes.iter_mut().find(|p| p.intent == intent) {
            pending.status = PendingStatus::Queued;
        }
        effects.writes.push(intent);
    }
}

/// The tearing guard: a set present last cycle but missing from this
/// otherwise-successful snapshot is carried forward once (as a suspect); only
/// a second consecutive absence lets it drop. A reappearing set clears its
/// suspicion naturally (this cycle's absences rebuild the suspect list).
/// Returns how many sets were dropped for real.
fn apply_tearing_guard(runtime: &mut BracketRuntime, sets: &mut Vec<LiveSet>) -> usize {
    let incoming: HashSet<&SetKey> = sets.iter().map(|s| &s.key).collect();
    let mut retained = Vec::new();
    let mut dropped = 0;
    for old in &runtime.state.sets {
        if incoming.contains(&old.key) {
            continue;
        }
        if runtime.suspects.contains(&old.key) {
            dropped += 1;
        } else {
            retained.push(old.clone());
        }
    }
    runtime.suspects = retained.iter().map(|s| s.key.clone()).collect();
    sets.extend(retained);
    dropped
}

/// A set the server reports finished releases its station automatically.
/// Returns how many setups freed (a freed setup is a decision point).
fn free_setups_holding(state: &mut AppState, pair: &(BracketId, SetKey), now: UnixMillis) -> usize {
    let held: Vec<SetupId> = state
        .board
        .setups()
        .iter()
        .filter(|s| match &s.status {
            SetupStatus::Called { bracket, set } | SetupStatus::InProgress { bracket, set } => (bracket, set) == (&pair.0, &pair.1),
            _ => false,
        })
        .map(|s| s.id)
        .collect();
    let freed = held.len();
    for setup in held {
        state.board.set_status(setup, SetupStatus::Free);
        state.notice(now, NoticeLevel::Info, format!("setup {} free (result arrived)", setup.0));
    }
    freed
}

/// Unknown state-int transitions: always advisory, optionally escalated to
/// soft-busy evidence (config). The soft-busy list mirrors the *current*
/// deviations per bracket, not an accumulating history.
fn ingest_deviations(state: &mut AppState, ix: usize, prev: &[LiveSet], now: UnixMillis) {
    let bracket_id = state.brackets[ix].state.id.clone();
    let known = state.known_ints();
    let prev_by_key: HashMap<&SetKey, &LiveSet> = prev.iter().map(|s| (&s.key, s)).collect();

    let mut deviations = Vec::new();
    for set in &state.brackets[ix].state.sets {
        let Some(baseline) = prev_by_key.get(&set.key) else { continue };
        if let Some(dev) = state_deviation(set, baseline, &known) {
            deviations.push(dev);
        }
    }

    state.soft_busy.retain(|(bracket, _)| bracket != &bracket_id);
    for dev in deviations {
        let text = format!(
            "{}: {:?} state {:?} -> {:?} (unrecognized)",
            bracket_id.0, dev.key, dev.from, dev.to
        );
        state.notice(now, NoticeLevel::Warn, text);
        if state.config.escalate_unpinned_state_deviation {
            state.soft_busy.push((bracket_id.clone(), dev.key));
        }
    }
}

/// Wait-time credit starts when a set first shows up ready to play (both
/// slots occupied, not finished, not already running).
fn stamp_ready(state: &mut AppState, ix: usize, now: UnixMillis) {
    let ready_keys: Vec<SetKey> = state.brackets[ix]
        .state
        .sets
        .iter()
        .filter(|s| !s.is_completed() && !s.is_remotely_active() && s.all_slots_occupied())
        .map(|s| s.key.clone())
        .collect();
    for key in ready_keys {
        state.callable_since.entry(key).or_insert(now);
    }
}

// --- write results

fn handle_write_result(state: &mut AppState, result: WriteResult, now: UnixMillis) {
    let intent = result.intent;
    match result.outcome {
        WriteOutcome::Success { payload, offset } => {
            state.pending_writes.retain(|p| p.intent != intent);
            if let Some(new_int) = payload.state {
                learn_state_int(state, &intent.kind, new_int, now);
            }
            if offset.is_some() {
                state.clock_offset = offset;
            }
            if let WriteKind::Report(report) = &intent.kind {
                let text = format!("report confirmed: {}", report.summary);
                state.notice(now, NoticeLevel::Info, text);
            }
            merge_remote_set(state, &intent, &payload);
            state.dirty = true;
        }
        WriteOutcome::Transient { error, attempts } => {
            if let Some(pending) = state.pending_writes.iter_mut().find(|p| p.intent == intent) {
                pending.attempts = attempts;
                pending.last_error = Some(error);
            }
        }
        WriteOutcome::AwaitReconnect { error } => {
            if let Some(pending) = state.pending_writes.iter_mut().find(|p| p.intent == intent) {
                pending.status = PendingStatus::AwaitingReconnect;
                pending.last_error = Some(error);
            }
            let text = format!(
                "write {} for set {} held until {} polls again",
                intent.kind.label(),
                intent.id,
                intent.bracket.0
            );
            state.notice(now, NoticeLevel::Warn, text);
            if !state.held_writes.contains(&intent) {
                state.held_writes.push(intent);
            }
        }
        WriteOutcome::Terminal { error } => {
            if let Some(pending) = state.pending_writes.iter_mut().find(|p| p.intent == intent) {
                pending.status = PendingStatus::Parked;
                pending.attempts += 1;
                pending.last_error = Some(error.clone());
            }
            let text = format!("write {} for set {} failed for good: {error}", intent.kind.label(), intent.id);
            state.notice(now, NoticeLevel::Error, text);
        }
    }
}

fn learn_state_int(state: &mut AppState, kind: &WriteKind, new_int: i32, now: UnixMillis) {
    let learned = match kind {
        WriteKind::Called => &mut state.called_ints,
        WriteKind::InProgress => &mut state.in_progress_ints,
        // A report's resulting state is COMPLETED, already benign by default.
        WriteKind::Report(_) => return,
    };
    if !learned.contains(&new_int) {
        learned.push(new_int);
        let text = format!("learned {} state int = {new_int}", kind.label());
        state.notice(now, NoticeLevel::Info, text);
    }
}

/// Fold the mutation payload into the local snapshot so the set doesn't look
/// deviant until the next poll confirms it.
fn merge_remote_set(state: &mut AppState, intent: &WriteIntent, remote: &SetMutationResult) {
    let Some(ix) = state.bracket_ix(&intent.bracket) else { return };
    let Some(set) = state.brackets[ix].state.sets.iter_mut().find(|s| s.key == intent.key) else {
        return;
    };
    if remote.state.is_some() {
        set.state_int = remote.state;
    }
    if let Some(started) = remote.started_at {
        set.started_at = Some(started.0);
    }
    if let Some(completed) = remote.completed_at {
        set.completed_at = Some(completed.0);
    }
}

// --- ticks

fn scan_no_shows(state: &mut AppState, now: UnixMillis) {
    let threshold = state.config.no_show_secs as i64 * 1000;
    let overdue: Vec<(BracketId, SetKey)> = state
        .board
        .setups()
        .iter()
        .filter_map(|s| match &s.status {
            SetupStatus::Called { bracket, set } => Some((bracket.clone(), set.clone())),
            _ => None,
        })
        .filter(|pair| state.called_at.get(pair).is_some_and(|&at| now - at > threshold) && !state.no_show_alerted.contains(pair))
        .collect();
    for pair in overdue {
        let players = state
            .find_set(&pair.0, &pair.1)
            .map(|s| s.occupants().map(|o| o.display_name.as_str()).collect::<Vec<_>>().join(" vs "))
            .unwrap_or_default();
        let text = format!("no-show timer expired: {players} ({})", pair.0 .0);
        state.notice(now, NoticeLevel::Warn, text);
        state.no_show_alerted.insert(pair);
        state.overlay_dirty = true;
    }
}

// --- recompute plumbing

fn recompute_world(state: &AppState, now: UnixMillis) -> World {
    let bracket_states: Vec<&BracketState> = state.brackets.iter().map(|b| &b.state).collect();
    let inputs = WorldInputs {
        brackets: &bracket_states,
        board: &state.board,
        flags: &state.flags,
        tombstones: &state.tombstones,
        aliases: &state.aliases,
        called_ints: &state.called_ints,
        soft_busy: &state.soft_busy,
        last_completed: &state.last_completed,
        snoozes: &state.snoozes,
        callable_since: &state.callable_since,
        pool_overrides: &state.pool_overrides,
        rest_window_secs: state.config.rest_window_secs,
        sim: state.config.sim.clone(),
        now_millis: now,
    };
    recompute(&inputs, &state.durations, &GreedyRanker)
}

/// Flattens a `(bracket, set) → millis` map to the vec-of-pairs the JSON
/// overlay stores (JSON object keys must be strings).
fn flatten_pair_map(map: &HashMap<(BracketId, SetKey), UnixMillis>) -> Vec<(BracketId, SetKey, UnixMillis)> {
    map.iter().map(|((b, k), v)| (b.clone(), k.clone(), *v)).collect()
}

/// Unions persisted state ints into the config-seeded list without duplicates.
fn merge_ints(into: &mut Vec<i32>, from: Vec<i32>) {
    for value in from {
        if !into.contains(&value) {
            into.push(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use bracket_tools_startgg::SetMutationResult;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::{
        recompute_world, setups_rows, update, AppState, BracketBootstrap, Modal, Msg, NoticeLevel, PendingStatus, PollFailure, PollOutcome,
        PollResult, SetupsRow, SimUrgency, WriteIntent, WriteKind, WriteOutcome, WriteResult, WORLD_REFRESH_MS,
    };
    use crate::{
        config::{BracketConfig, BracketMode, OneOrMany, SchedulerConfig, SetupCounts, SetupId, DEFAULT_SETUP_TYPE},
        conflict::{BlockReason, PoolOverride, SetupBoard, SetupStatus},
        fixture_source::FixtureSource,
        model::{live_sets_from_schema, BracketId, LiveSet, PlayerId},
        set_source::SetSource,
        synth::{complete, make_de_bracket_with, make_se_bracket, materialize_ids, SynthBracket, SynthPlayer},
    };

    const NOW: i64 = 1_751_000_000_000;

    fn key(code: KeyCode) -> Msg {
        Msg::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn test_config(setups: &[u32], brackets: &[&str]) -> SchedulerConfig {
        SchedulerConfig {
            setups: Some(SetupCounts::Uniform(setups.len() as u32)),
            brackets: brackets.iter().map(|slug| BracketConfig::new(*slug)).collect(),
            known_called_state_int: Some(6),
            known_in_progress_state_int: Some(2),
            ..SchedulerConfig::default()
        }
    }

    fn bootstrap(brackets: Vec<(&str, &SynthBracket)>) -> Vec<BracketBootstrap> {
        brackets
            .into_iter()
            .map(|(slug, bracket)| BracketBootstrap {
                id: BracketId(slug.to_owned()),
                sets: bracket.sets.clone(),
                groups: vec![bracket.info.clone()],
                mode: BracketMode::Full,
                start_at: None,
                setup_types: vec![DEFAULT_SETUP_TYPE.to_owned()],
                duration_prior_secs: 480,
                prior_weight: 4.0,
                characters: Vec::new(),
            })
            .collect()
    }

    /// A materialized (numeric-id) 4-player SE bracket: R1 A + B, final C.
    fn se4() -> SynthBracket {
        let mut bracket = make_se_bracket(1001, 4);
        bracket.sets = materialize_ids(&bracket.sets, 9000);
        bracket
    }

    fn se4_app(writes_armed: bool) -> AppState {
        let config = test_config(&[1, 2], &["ultimate"]);
        let boots = bootstrap(vec![("ultimate", &se4())]);
        AppState::new(config, writes_armed, boots, NOW)
    }

    fn snapshot_msg(bracket: &str, seq: u64, sets: Vec<LiveSet>) -> Msg {
        Msg::Poll(PollResult {
            bracket: BracketId(bracket.to_owned()),
            seq,
            captured_at: NOW + seq as i64 * 30_000,
            outcome: PollOutcome::Snapshot {
                sets,
                warnings: Vec::new(),
                skipped: Vec::new(),
            },
        })
    }

    /// Winner of `source` advances into the dependent slot that references it
    /// (what the server does between polls).
    fn propagate_winner(sets: &mut [LiveSet], source_ix: usize) {
        let source = sets[source_ix].clone();
        let winner = source
            .winner_id
            .as_ref()
            .and_then(|w| source.occupants().find(|o| &o.entrant_id == w))
            .expect("completed set has a winner occupant")
            .clone();
        for set in sets.iter_mut() {
            for slot in &mut set.slots {
                if let Some(crate::model::Prereq::Set { id, placement }) = &slot.prereq {
                    if id == &source.id && *placement == Some(1) {
                        slot.occupant = Some(winner.clone());
                    }
                }
            }
        }
    }

    fn call_top_candidate(state: &mut AppState, setup_digit: char) -> super::UpdateEffects {
        update(state, key(KeyCode::Char(setup_digit)), NOW);
        assert!(matches!(state.ui.modal, Some(Modal::CallPicker { .. })), "picker should open");
        update(state, key(KeyCode::Enter), NOW)
    }

    #[test]
    fn full_cycle_call_complete_free_rerank() {
        let mut state = se4_app(true);
        assert_eq!(state.world.queue.len(), 2, "both R1 sets callable");

        // Call the top candidate (deterministically set A) on setup 1.
        let effects = call_top_candidate(&mut state, '1');
        let called_key = {
            let Some(SetupStatus::Called { set, .. }) = state.board.setups().iter().find(|s| s.id == SetupId(1)).map(|s| s.status.clone())
            else {
                panic!("setup 1 should be Called");
            };
            set
        };
        assert_eq!(effects.writes.len(), 1);
        assert_eq!(effects.writes[0].kind, WriteKind::Called);
        assert_eq!(state.pending_writes.len(), 1);

        // The called set leaves the queue; its players block nothing else in
        // this bracket (they appear only in A), so B remains.
        assert_eq!(state.world.queue.len(), 1);
        assert!(state.world.queue.iter().all(|e| e.key != called_key));

        // p: mark in progress.
        let effects = update(&mut state, key(KeyCode::Char('p')), NOW);
        assert_eq!(effects.writes.len(), 1);
        assert_eq!(effects.writes[0].kind, WriteKind::InProgress);

        // Poll: A completed, winner advanced into the final.
        let mut next = state.brackets[0].state.sets.clone();
        let a_ix = next.iter().position(|s| s.key == called_key).unwrap();
        complete(&mut next[a_ix], 0, NOW / 1000 + 900);
        propagate_winner(&mut next, a_ix);
        update(&mut state, snapshot_msg("ultimate", 1, next), NOW + 60_000);

        // Result arrival frees the setup and ingests a duration sample.
        let setup1 = state.board.setups().iter().find(|s| s.id == SetupId(1)).unwrap().status.clone();
        assert_eq!(setup1, SetupStatus::Free);
        assert_eq!(state.durations.sample_count(&BracketId("ultimate".to_owned())), 1);

        // Re-rank: B is still callable; the final waits on B's winner.
        assert_eq!(state.world.queue.len(), 1);
        let final_blocked = state
            .world
            .blocked
            .iter()
            .find(|((_, key), _)| key.round == 2)
            .map(|(_, reasons)| reasons.clone())
            .expect("final is blocked");
        assert!(final_blocked.iter().any(|r| matches!(r, BlockReason::SlotsUnresolved)));
    }

    #[test]
    fn no_double_booking_while_called_or_in_progress() {
        // Same 8 players in both brackets.
        let players: Vec<SynthPlayer> = (1..=8)
            .map(|i| SynthPlayer {
                player_id: format!("P{i}"),
                name: format!("Player {i}"),
            })
            .collect();
        let ultimate = make_de_bracket_with(1001, &players);
        let melee = make_de_bracket_with(2001, &players);
        let config = test_config(&[1, 2, 3], &["ultimate", "melee"]);
        let boots = bootstrap(vec![("ultimate", &ultimate), ("melee", &melee)]);
        let mut state = AppState::new(config, false, boots, NOW);

        let effects = call_top_candidate(&mut state, '1');
        assert!(effects.writes.is_empty(), "writes disarmed");
        let busy_players: Vec<String> = {
            let Some(SetupStatus::Called { bracket, set }) =
                state.board.setups().iter().find(|s| s.id == SetupId(1)).map(|s| s.status.clone())
            else {
                panic!("setup 1 should be Called");
            };
            state
                .find_set(&bracket, &set)
                .unwrap()
                .occupants()
                .map(|o| o.display_name.clone())
                .collect()
        };

        // Nobody in the called set may appear anywhere in the queue — the
        // same humans play both brackets.
        for entry in &state.world.queue {
            for name in entry.players.split(" vs ") {
                assert!(!busy_players.iter().any(|b| b == name), "double-booked {name}: {entry:?}");
            }
        }
        // And the mirrored set in the other bracket is blocked by PlayerBusy.
        assert!(state
            .world
            .blocked
            .values()
            .any(|reasons| reasons.iter().any(|r| matches!(r, BlockReason::PlayerBusy { .. }))));
    }

    #[test]
    fn alias_linked_players_block_across_brackets() {
        let ultimate = make_de_bracket_with(1001, &crate::synth::default_players(4));
        let melee_players: Vec<SynthPlayer> = (1..=4)
            .map(|i| SynthPlayer {
                player_id: format!("M{i}"),
                name: format!("Melee {i}"),
            })
            .collect();
        let melee = make_de_bracket_with(2001, &melee_players);
        let mut config = test_config(&[1, 2], &["ultimate", "melee"]);
        // P1 (ultimate) and M1 (melee) are the same human.
        config.player_aliases = vec![vec![PlayerId("P1".to_owned()), PlayerId("M1".to_owned())]];
        let boots = bootstrap(vec![("ultimate", &ultimate), ("melee", &melee)]);
        let mut state = AppState::new(config, false, boots, NOW);

        // Find and call P1's ultimate set via the picker.
        update(&mut state, key(KeyCode::Char('1')), NOW);
        let Some(Modal::CallPicker { setup, .. }) = state.ui.modal.clone() else {
            panic!("picker open");
        };
        let p1_pos = state.world.per_setup[&setup]
            .iter()
            .position(|e| {
                state
                    .find_set(&e.bracket, &e.key)
                    .unwrap()
                    .occupants()
                    .any(|o| o.player_ids.iter().any(|p| p.0 == "P1"))
            })
            .expect("P1 has a callable set");
        for _ in 0..p1_pos {
            update(&mut state, key(KeyCode::Down), NOW);
        }
        update(&mut state, key(KeyCode::Enter), NOW);

        // M1's melee set must now be blocked through the alias.
        assert!(
            !state
                .world
                .queue
                .iter()
                .any(|e| e.bracket.0 == "melee" && e.players.contains("Melee 1")),
            "alias-linked player double-booked: {:?}",
            state.world.queue
        );
    }

    #[test]
    fn out_of_order_snapshot_is_rejected() {
        let mut state = se4_app(false);
        let initial = state.brackets[0].state.sets.clone();

        let mut newer = initial.clone();
        complete(&mut newer[0], 0, NOW / 1000 + 600);
        update(&mut state, snapshot_msg("ultimate", 2, newer), NOW + 30_000);
        let completed_after_seq2 = state.brackets[0].state.sets.iter().filter(|s| s.is_completed()).count();
        assert_eq!(completed_after_seq2, 1);

        // A stale seq-1 snapshot (nothing completed) must not roll back.
        update(&mut state, snapshot_msg("ultimate", 1, initial), NOW + 60_000);
        let completed_after_stale = state.brackets[0].state.sets.iter().filter(|s| s.is_completed()).count();
        assert_eq!(completed_after_stale, 1, "stale snapshot applied");
        assert_eq!(state.brackets[0].applied_seq, 2);
    }

    #[test]
    fn queue_is_deterministic_between_recomputes() {
        // Same instant, repeated recomputes: byte-identical queues. (Direct
        // recomputes — a bare tick only refreshes once the world is stale.)
        let state = se4_app(false);
        let first = recompute_world(&state, NOW + 1000).queue;
        assert_eq!(first, recompute_world(&state, NOW + 1000).queue);

        // Later instant: only the wait-time credit may move; the ordering
        // identity must not.
        let later = recompute_world(&state, NOW + 60_000).queue;
        let order = |entries: &[crate::world::QueueEntry]| entries.iter().map(|e| e.key.clone()).collect::<Vec<_>>();
        assert_eq!(order(&first), order(&later));
    }

    #[test]
    fn bare_ticks_recompute_only_once_stale() {
        let mut state = se4_app(false);
        update(&mut state, Msg::Tick, NOW);
        let stamped = state.last_recompute;
        assert_eq!(stamped, NOW, "first tick computes the initial world");

        update(&mut state, Msg::Tick, NOW + 1000);
        assert_eq!(state.last_recompute, stamped, "fresh world: tick skips the recompute");

        update(&mut state, Msg::Tick, NOW + WORLD_REFRESH_MS);
        assert_eq!(state.last_recompute, NOW + WORLD_REFRESH_MS, "stale world: tick refreshes");
    }

    #[test]
    fn undo_restores_the_board() {
        let mut state = se4_app(false);
        call_top_candidate(&mut state, '1');
        assert!(matches!(state.board.setups()[0].status, SetupStatus::Called { .. }));

        update(&mut state, key(KeyCode::Char('u')), NOW);
        assert_eq!(state.board.setups()[0].status, SetupStatus::Free);
        assert_eq!(state.world.queue.len(), 2, "the un-called set ranks again");
        assert!(state.notices.iter().any(|n| n.text.starts_with("undid:")));
    }

    #[test]
    fn snooze_hides_then_expires() {
        let mut state = se4_app(false);
        update(&mut state, key(KeyCode::Char('z')), NOW);
        assert_eq!(state.world.queue.len(), 1, "snoozed set hidden");

        // Just before expiry: still hidden. After (plus the tick refresh
        // cadence — bare ticks recompute at most every WORLD_REFRESH_MS):
        // back.
        update(&mut state, Msg::Tick, NOW + (super::SNOOZE_SECS - 1) * 1000);
        assert_eq!(state.world.queue.len(), 1);
        update(&mut state, Msg::Tick, NOW + super::SNOOZE_SECS * 1000 + WORLD_REFRESH_MS);
        assert_eq!(state.world.queue.len(), 2);
    }

    #[test]
    fn preview_ids_never_enqueue_writes() {
        let config = test_config(&[1], &["ultimate"]);
        let preview = make_se_bracket(1001, 4); // preview_* ids
        let boots = bootstrap(vec![("ultimate", &preview)]);
        let mut state = AppState::new(config, true, boots, NOW);

        let effects = call_top_candidate(&mut state, '1');
        assert!(effects.writes.is_empty());
        assert!(state.pending_writes.is_empty());
        assert!(state.notices.iter().any(|n| n.text.contains("preview id")));
        assert!(matches!(state.board.setups()[0].status, SetupStatus::Called { .. }));
    }

    #[test]
    fn write_results_learn_state_ints_and_park_failures() {
        let mut state = se4_app(true);
        let effects = call_top_candidate(&mut state, '1');
        let intent = effects.writes[0].clone();

        // Success teaches an unseen int and lands the offset sample.
        update(
            &mut state,
            Msg::Write(WriteResult {
                intent: intent.clone(),
                outcome: WriteOutcome::Success {
                    payload: SetMutationResult {
                        id: Some(intent.id),
                        state: Some(42),
                        started_at: None,
                        completed_at: None,
                    },
                    offset: Some(super::OffsetSample {
                        offset_secs: 3,
                        at: NOW + 900,
                    }),
                },
            }),
            NOW + 1000,
        );
        assert!(state.called_ints.contains(&42));
        assert!(state.pending_writes.is_empty());
        assert_eq!(state.clock_offset.map(|s| s.offset_secs), Some(3));
        // The local set mirrors the confirmed state int.
        let set = state.find_set(&intent.bracket, &intent.key).unwrap();
        assert_eq!(set.state_int, Some(42));

        // Terminal failure parks the pending entry.
        update(&mut state, key(KeyCode::Char('u')), NOW + 2000); // free the setup again
        let effects = call_top_candidate(&mut state, '1');
        let intent2 = effects.writes[0].clone();
        update(
            &mut state,
            Msg::Write(WriteResult {
                intent: intent2,
                outcome: WriteOutcome::Terminal {
                    error: "418 teapot".to_owned(),
                },
            }),
            NOW + 3000,
        );
        assert!(state.pending_writes.iter().any(|p| p.status == PendingStatus::Parked));
        assert!(state.notices.iter().any(|n| n.level == NoticeLevel::Error));
    }

    #[test]
    fn free_requests_a_targeted_force_poll() {
        let mut state = se4_app(false);
        call_top_candidate(&mut state, '1');
        let effects = update(&mut state, key(KeyCode::Char('f')), NOW + 1000);

        assert_eq!(effects.force_poll, vec![BracketId("ultimate".to_owned())]);
        assert_eq!(state.board.setups()[0].status, SetupStatus::Free);
        // Awaiting-completion tombstone keeps the set out of the queue.
        assert_eq!(state.world.queue.len(), 1);
        assert!(!state.tombstones.awaiting_remote_completion.is_empty());
    }

    #[test]
    fn requeue_returns_the_set_to_the_queue() {
        let mut state = se4_app(false);
        call_top_candidate(&mut state, '1');
        assert_eq!(state.world.queue.len(), 1);

        update(&mut state, key(KeyCode::Char('r')), NOW + 1000);
        assert_eq!(state.board.setups()[0].status, SetupStatus::Free);
        assert_eq!(state.world.queue.len(), 2, "re-queued set ranks again");
    }

    #[test]
    fn quit_keys_quit() {
        let mut state = se4_app(false);
        assert!(update(&mut state, key(KeyCode::Char('q')), NOW).quit);
        assert!(update(&mut state, Msg::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)), NOW).quit);
    }

    #[test]
    fn poll_failures_track_health_without_touching_sets() {
        let mut state = se4_app(false);
        let before = state.brackets[0].state.sets.clone();
        update(
            &mut state,
            Msg::Poll(PollResult {
                bracket: BracketId("ultimate".to_owned()),
                seq: 1,
                captured_at: NOW,
                outcome: PollOutcome::Failed(PollFailure::Persistent("403".to_owned())),
            }),
            NOW,
        );
        assert_eq!(state.brackets[0].consecutive_failures, 1);
        assert!(matches!(state.brackets[0].health, super::PollHealth::Persistent(_)));
        assert_eq!(state.brackets[0].state.sets, before);
        assert!(state.notices.iter().any(|n| n.level == NoticeLevel::Error));
    }

    #[test]
    fn no_show_alert_fires_once() {
        let mut state = se4_app(false);
        call_top_candidate(&mut state, '1');

        let after = NOW + (state.config.no_show_secs as i64 + 1) * 1000;
        update(&mut state, Msg::Tick, after);
        let count = |s: &AppState| s.notices.iter().filter(|n| n.text.contains("no-show")).count();
        assert_eq!(count(&state), 1);
        update(&mut state, Msg::Tick, after + 1000);
        assert_eq!(count(&state), 1, "alert must not repeat");
    }

    #[test]
    fn single_flight_write_per_set() {
        let mut state = se4_app(true);
        let effects = call_top_candidate(&mut state, '1');
        let intent = effects.writes[0].clone();

        // A second identical enqueue attempt (e.g. p on a set whose Called
        // write is still pending) reuses the pending slot for that id.
        let mut effects2 = super::UpdateEffects::default();
        super::enqueue_write(
            &mut state,
            &mut effects2,
            &intent.bracket.clone(),
            &intent.key.clone(),
            &intent.id.to_string(),
            WriteKind::Called,
            NOW + 500,
        );
        assert!(effects2.writes.is_empty());
        assert_eq!(state.pending_writes.len(), 1);
    }

    #[test]
    fn write_intents_expose_key_and_creation_time() {
        let mut state = se4_app(true);
        let effects = call_top_candidate(&mut state, '1');
        let WriteIntent { key, created_at, .. } = effects.writes[0].clone();
        assert_eq!(created_at, NOW);
        assert_eq!(key.phase_group, "1001");
    }

    /// The full seam: a scripted FixtureSource timeline round-trips through
    /// the schema layer and drives update() end to end.
    #[tokio::test]
    async fn fixture_source_timeline_drives_update() {
        let slug = "tournament/synth/event/ultimate";
        let bracket = se4();
        let mut second = bracket.sets.clone();
        complete(&mut second[0], 0, NOW / 1000 + 900);
        propagate_winner(&mut second, 0);

        let mut source = FixtureSource::new();
        source.add_synth_event(slug, std::slice::from_ref(&bracket.info), vec![bracket.sets.clone(), second]);

        async fn fetch(source: &FixtureSource, slug: &str) -> Vec<LiveSet> {
            let (sets, warnings, skipped) = live_sets_from_schema(source.fetch_event_sets(slug).await.unwrap());
            assert!(warnings.is_empty() && skipped.is_empty());
            sets
        }

        let config = test_config(&[1, 2], &[slug]);
        let initial = fetch(&source, slug).await;
        let boots = vec![BracketBootstrap {
            id: BracketId(slug.to_owned()),
            sets: initial,
            groups: vec![bracket.info.clone()],
            mode: BracketMode::Full,
            start_at: None,
            setup_types: vec![DEFAULT_SETUP_TYPE.to_owned()],
            duration_prior_secs: 480,
            prior_weight: 4.0,
            characters: Vec::new(),
        }];
        let mut state = AppState::new(config, true, boots, NOW);
        assert_eq!(state.world.queue.len(), 2);

        // Call the top candidate, then replay the "A finished" snapshot.
        call_top_candidate(&mut state, '1');
        assert_eq!(state.world.queue.len(), 1);
        let next = fetch(&source, slug).await;
        update(&mut state, snapshot_msg(slug, 1, next), NOW + 60_000);

        assert_eq!(state.board.setups()[0].status, SetupStatus::Free, "result arrival frees the setup");
        assert_eq!(state.durations.sample_count(&BracketId(slug.to_owned())), 1);
        assert_eq!(state.world.queue.len(), 1, "B still callable; the final waits on B");
    }

    #[test]
    fn tearing_guard_retains_vanished_set_for_one_cycle() {
        let mut state = se4_app(false);
        let full = state.brackets[0].state.sets.clone();
        let victim = full[0].key.clone();
        let torn: Vec<LiveSet> = full.iter().filter(|s| s.key != victim).cloned().collect();

        // A single torn snapshot doesn't lose the set.
        update(&mut state, snapshot_msg("ultimate", 1, torn.clone()), NOW + 30_000);
        assert!(state.brackets[0].state.sets.iter().any(|s| s.key == victim), "retained one cycle");
        assert_eq!(state.world.queue.len(), 2, "retained set still ranks");

        // Reappearing clears the suspicion.
        update(&mut state, snapshot_msg("ultimate", 2, full.clone()), NOW + 60_000);
        assert!(state.brackets[0].state.sets.iter().any(|s| s.key == victim));

        // Two consecutive absences drop it for real, with one aggregate notice.
        update(&mut state, snapshot_msg("ultimate", 3, torn.clone()), NOW + 90_000);
        assert!(
            state.brackets[0].state.sets.iter().any(|s| s.key == victim),
            "first absence: suspect"
        );
        update(&mut state, snapshot_msg("ultimate", 4, torn), NOW + 120_000);
        assert!(
            !state.brackets[0].state.sets.iter().any(|s| s.key == victim),
            "second absence: gone"
        );
        assert!(state.notices.iter().any(|n| n.text.contains("removed server-side")));
        assert_eq!(state.world.queue.len(), 1);
    }

    #[test]
    fn reconnect_held_write_releases_on_fresh_poll_and_drops_when_moot() {
        let mut state = se4_app(true);
        let effects = call_top_candidate(&mut state, '1');
        let intent = effects.writes[0].clone();

        // Writer reports a connectivity failure: the intent is held.
        update(
            &mut state,
            Msg::Write(WriteResult {
                intent: intent.clone(),
                outcome: WriteOutcome::AwaitReconnect {
                    error: "request timed out".to_owned(),
                },
            }),
            NOW + 1000,
        );
        assert_eq!(state.held_writes.len(), 1);
        assert!(state.pending_writes.iter().any(|p| p.status == PendingStatus::AwaitingReconnect));

        // A successful poll of the target's event (newer than the intent)
        // revalidates and re-releases it to the writer.
        let sets = state.brackets[0].state.sets.clone();
        let effects = update(&mut state, snapshot_msg("ultimate", 1, sets), NOW + 30_000);
        assert_eq!(effects.writes, vec![intent.clone()]);
        assert!(state.held_writes.is_empty());
        assert!(state.pending_writes.iter().all(|p| p.status == PendingStatus::Queued));

        // Held again, but this time the set completes remotely before the
        // poll: the intent is moot and drops with a notice.
        update(
            &mut state,
            Msg::Write(WriteResult {
                intent: intent.clone(),
                outcome: WriteOutcome::AwaitReconnect {
                    error: "request timed out".to_owned(),
                },
            }),
            NOW + 31_000,
        );
        let mut next = state.brackets[0].state.sets.clone();
        let ix = next.iter().position(|s| s.key == intent.key).unwrap();
        complete(&mut next[ix], 0, NOW / 1000 + 600);
        let effects = update(&mut state, snapshot_msg("ultimate", 2, next), NOW + 60_000);
        assert!(effects.writes.is_empty(), "moot intent must not re-send");
        assert!(state.held_writes.is_empty());
        assert!(state.pending_writes.is_empty(), "moot intent dropped from pending");
        assert!(state.notices.iter().any(|n| n.text.contains("dropped held write")));
    }

    #[test]
    fn overlay_round_trip_restores_local_state() {
        let mut state = se4_app(true);
        call_top_candidate(&mut state, '1');
        let called = match state.board.setups().iter().find(|s| s.id == SetupId(1)).unwrap().status.clone() {
            SetupStatus::Called { set, .. } => set,
            other => panic!("setup 1 should be Called, got {other:?}"),
        };
        assert_eq!(state.pending_writes.len(), 1);

        // Snapshot, then rehydrate onto a fresh (all-Free) instance.
        let doc = state.to_overlay();
        let mut restored = se4_app(true);
        assert_eq!(restored.board.setups()[0].status, SetupStatus::Free);
        restored.apply_overlay(doc, NOW, true);

        match restored.board.setups().iter().find(|s| s.id == SetupId(1)).unwrap().status.clone() {
            SetupStatus::Called { set, .. } => assert_eq!(set, called),
            other => panic!("restored setup 1 should be Called, got {other:?}"),
        }
        assert!(restored.called_at.keys().any(|(_, k)| k == &called), "called_at restored");
        assert!(restored.called_ints.contains(&6), "config-pinned int survives");
        // The in-flight write is parked for the TO, not silently re-queued.
        assert_eq!(restored.pending_writes.len(), 1);
        assert_eq!(restored.pending_writes[0].status, PendingStatus::Parked);
        // The re-ranked world matches: the called set is out of the queue.
        assert!(restored.world.queue.iter().all(|e| e.key != called));
    }

    #[test]
    fn setups_modal_adds_and_undo_restores() {
        let mut state = se4_app(false);
        update(&mut state, key(KeyCode::Char('s')), NOW);
        assert!(matches!(state.ui.modal, Some(Modal::Setups { selected: 0 })));
        assert_eq!(
            setups_rows(&state),
            vec![
                SetupsRow::Retire(SetupId(1), "default".to_owned()),
                SetupsRow::Retire(SetupId(2), "default".to_owned()),
                SetupsRow::Add("default".to_owned()),
            ]
        );

        // Enter on the add row: setup 3 appears, the modal stays open, and
        // the roster change demands an immediate rollout re-evaluation.
        update(&mut state, key(KeyCode::Down), NOW);
        update(&mut state, key(KeyCode::Down), NOW);
        let effects = update(&mut state, key(KeyCode::Enter), NOW);
        assert_eq!(effects.sim, Some(SimUrgency::Immediate));
        assert!(
            matches!(state.ui.modal, Some(Modal::Setups { .. })),
            "modal stays open for batch adds"
        );
        let ids: Vec<u32> = state.board.setups().iter().map(|s| s.id.0).collect();
        assert_eq!(ids, vec![1, 2, 3]);
        assert!(state.world.per_setup.contains_key(&SetupId(3)), "the new station ranks immediately");

        // Esc + u: the whole roster edit rolls back.
        update(&mut state, key(KeyCode::Esc), NOW);
        update(&mut state, key(KeyCode::Char('u')), NOW);
        assert_eq!(state.board.setups().len(), 2, "undo restores the roster");
    }

    #[test]
    fn setups_modal_retires_free_stations_only() {
        let mut state = se4_app(true);
        call_top_candidate(&mut state, '1');
        state.pool_overrides.insert(SetupId(2), PoolOverride::AllowAny);
        state.ui.selected_setup = Some(SetupId(2));
        update(&mut state, key(KeyCode::Char('s')), NOW);

        // Row 0 is the occupied setup 1: Enter refuses with a warning.
        update(&mut state, key(KeyCode::Enter), NOW);
        assert_eq!(state.board.setups().len(), 2, "occupied stations don't retire");
        assert!(state.notices.iter().any(|n| n.text.contains("occupied")));

        // Row 1 is the free setup 2: retired, its override and selection go
        // with it (a stale Dedicated must not re-attach to a reused number).
        update(&mut state, key(KeyCode::Down), NOW);
        let effects = update(&mut state, key(KeyCode::Enter), NOW);
        assert_eq!(effects.sim, Some(SimUrgency::Immediate));
        let ids: Vec<u32> = state.board.setups().iter().map(|s| s.id.0).collect();
        assert_eq!(ids, vec![1]);
        assert!(state.pool_overrides.is_empty(), "stale override removed");
        assert_eq!(state.ui.selected_setup, None, "selection cleared");
    }

    #[test]
    fn setups_modal_add_reuses_the_lowest_retired_number() {
        let mut state = se4_app(false);
        update(&mut state, key(KeyCode::Char('s')), NOW);
        // Retire setup 1 (row 0, free), then add: the arrival becomes the
        // new setup 1 (physical placard reuse).
        update(&mut state, key(KeyCode::Enter), NOW);
        assert_eq!(state.board.setups().len(), 1);
        update(&mut state, key(KeyCode::Down), NOW);
        update(&mut state, key(KeyCode::Enter), NOW);
        let ids: Vec<u32> = state.board.setups().iter().map(|s| s.id.0).collect();
        assert_eq!(ids, vec![1, 2]);
    }

    #[test]
    fn setups_modal_seeds_a_zero_station_type() {
        let melee = se4();
        let config = SchedulerConfig {
            setups: Some(SetupCounts::ByType(BTreeMap::from([("switch".to_owned(), 1)]))),
            brackets: vec![
                BracketConfig {
                    setup_type: Some(OneOrMany::One("switch".to_owned())),
                    ..BracketConfig::new("melee")
                },
                BracketConfig {
                    setup_type: Some(OneOrMany::One("pokemon".to_owned())),
                    ..BracketConfig::new("pokemon")
                },
            ],
            ..SchedulerConfig::default()
        };
        let boots = vec![
            BracketBootstrap {
                id: BracketId("melee".to_owned()),
                sets: melee.sets.clone(),
                groups: vec![melee.info.clone()],
                mode: BracketMode::Full,
                start_at: None,
                setup_types: vec!["switch".to_owned()],
                duration_prior_secs: 480,
                prior_weight: 4.0,
                characters: Vec::new(),
            },
            BracketBootstrap {
                id: BracketId("pokemon".to_owned()),
                sets: Vec::new(),
                groups: Vec::new(),
                mode: BracketMode::Full,
                start_at: None,
                setup_types: vec!["pokemon".to_owned()],
                duration_prior_secs: 480,
                prior_weight: 4.0,
                characters: Vec::new(),
            },
        ];
        let mut state = AppState::new(config, false, boots, NOW);
        assert!(
            state
                .notices
                .iter()
                .any(|n| n.text.contains("pokemon") && n.text.contains("no stations")),
            "zero-station type warned at launch: {:?}",
            state.notices
        );

        // The modal still offers pokemon's add row; Enter seeds station 2.
        update(&mut state, key(KeyCode::Char('s')), NOW);
        let rows = setups_rows(&state);
        let pokemon_add = rows.iter().position(|r| r == &SetupsRow::Add("pokemon".to_owned())).unwrap();
        for _ in 0..pokemon_add {
            update(&mut state, key(KeyCode::Down), NOW);
        }
        update(&mut state, key(KeyCode::Enter), NOW);
        let added = state.board.setups().iter().find(|s| s.setup_type == "pokemon").unwrap();
        assert_eq!(added.id, SetupId(2));
    }

    #[test]
    fn overlay_adoption_restores_a_runtime_grown_roster() {
        let mut state = se4_app(true);
        state.board.add_setup(SetupId(3), "pokemon".to_owned());
        let doc = state.to_overlay();

        let mut restored = se4_app(true);
        assert_eq!(restored.board.setups().len(), 2);
        restored.apply_overlay(doc, NOW, true);
        assert_eq!(restored.board.setups().len(), 3, "the runtime-added station survives a restart");
        assert_eq!(restored.board.setups()[2].setup_type, "pokemon");
        // Nothing in the config references "pokemon" — flagged, not dropped.
        assert!(restored.notices.iter().any(|n| n.text.contains("no bracket references")));
    }

    #[test]
    fn overlay_roster_is_ignored_when_counts_are_pinned() {
        let mut state = se4_app(true);
        call_top_candidate(&mut state, '1');
        state.board.add_setup(SetupId(3), "default".to_owned());
        let doc = state.to_overlay();

        let mut restored = se4_app(true);
        restored.apply_overlay(doc, NOW, false);
        assert_eq!(restored.board.setups().len(), 2, "--setups pinned the roster");
        assert!(
            matches!(restored.board.setups()[0].status, SetupStatus::Called { .. }),
            "statuses still re-key by id"
        );
        assert!(restored
            .notices
            .iter()
            .any(|n| n.text.contains("dropped persisted state for setup 3")));
    }

    #[test]
    fn pre_migration_overlay_rekeys_statuses_onto_the_config_roster() {
        let mut state = se4_app(true);
        call_top_candidate(&mut state, '1');
        let mut doc = state.to_overlay();
        // Simulate an overlay written before setup types existed: same
        // statuses, untyped stations (serde-defaulted to "").
        let mut old_board = SetupBoard::from_roster(&[(SetupId(1), String::new()), (SetupId(2), String::new())]);
        for setup in doc.board.setups() {
            old_board.set_status(setup.id, setup.status.clone());
        }
        doc.board = old_board;

        let mut restored = se4_app(true);
        restored.apply_overlay(doc, NOW, true);
        let restored_setup = &restored.board.setups()[0];
        assert_eq!(
            restored_setup.setup_type, DEFAULT_SETUP_TYPE,
            "config roster kept, not the untyped one"
        );
        assert!(matches!(restored_setup.status, SetupStatus::Called { .. }), "status re-keyed by id");
    }

    #[test]
    fn overlay_statuses_naming_an_unknown_bracket_reset_to_free() {
        let mut state = se4_app(true);
        // A status from another tournament's session (the shared XDG file).
        state.board.set_status(
            SetupId(2),
            SetupStatus::InProgress {
                bracket: BracketId("tournament/last-week/event/melee".to_owned()),
                set: state.brackets[0].state.sets[0].key.clone(),
            },
        );
        let doc = state.to_overlay();

        let mut restored = se4_app(true);
        restored.apply_overlay(doc, NOW, true);
        assert_eq!(restored.board.setups()[1].status, SetupStatus::Free, "foreign set freed");
        assert!(restored.notices.iter().any(|n| n.text.contains("outside this config")));
    }

    #[test]
    fn player_flags_modal_cycles_and_blocks() {
        let mut state = se4_app(false);
        assert_eq!(state.world.queue.len(), 2);

        // d opens the flags modal for the highlighted entry's players.
        update(&mut state, key(KeyCode::Char('d')), NOW);
        let Some(Modal::PlayerFlags { ref players, .. }) = state.ui.modal else {
            panic!("flags modal should open: {:?}", state.ui.modal);
        };
        assert_eq!(players.len(), 2, "singles set has two players");

        // Enter: resting. The player's set leaves the queue.
        update(&mut state, key(KeyCode::Enter), NOW);
        assert_eq!(state.flags.resting.len(), 1);
        assert_eq!(state.world.queue.len(), 1, "resting player's set blocked");
        assert!(state
            .world
            .blocked
            .values()
            .any(|reasons| reasons.iter().any(|r| matches!(r, BlockReason::PlayerResting { .. }))));

        // Cycle on: departed → force-available → clear restores the queue.
        update(&mut state, key(KeyCode::Enter), NOW);
        assert_eq!(state.flags.departed.len(), 1);
        update(&mut state, key(KeyCode::Enter), NOW);
        assert_eq!(state.flags.force_available.len(), 1);
        update(&mut state, key(KeyCode::Enter), NOW);
        assert!(state.flags.force_available.is_empty());
        assert_eq!(state.world.queue.len(), 2, "cleared flags unblock");

        // Undo restores the last flag state (single level).
        update(&mut state, key(KeyCode::Esc), NOW);
        update(&mut state, key(KeyCode::Char('u')), NOW);
        assert_eq!(state.flags.force_available.len(), 1, "undo restored the pre-clear flags");
    }

    #[test]
    fn sim_results_follow_the_one_refresh_modal_policy() {
        let mut state = se4_app(false);
        let rankings_at = |at: i64, state: &AppState| crate::world::RolloutRankings {
            per_setup: state
                .world
                .per_setup
                .iter()
                .map(|(setup, entries)| {
                    (
                        *setup,
                        entries
                            .iter()
                            .cloned()
                            .map(|e| crate::world::RolloutRow::Call(Box::new(e)))
                            .collect(),
                    )
                })
                .collect(),
            computed_at: at,
        };

        // No modal open: applies directly and drives the picker + effects.
        let first = rankings_at(NOW, &state);
        update(&mut state, Msg::SimResult(first.clone()), NOW);
        assert_eq!(state.rollout.as_ref().map(|r| r.computed_at), Some(NOW));

        // Open the picker: it consumes ONE refresh (marked), then holds back.
        update(&mut state, key(KeyCode::Char('1')), NOW);
        assert!(matches!(state.ui.modal, Some(Modal::CallPicker { refreshed: false, .. })));
        let second = rankings_at(NOW + 1000, &state);
        update(&mut state, Msg::SimResult(second), NOW + 1000);
        assert!(matches!(state.ui.modal, Some(Modal::CallPicker { refreshed: true, .. })));
        assert_eq!(state.rollout.as_ref().map(|r| r.computed_at), Some(NOW + 1000));
        let third = rankings_at(NOW + 2000, &state);
        update(&mut state, Msg::SimResult(third), NOW + 2000);
        assert_eq!(
            state.rollout.as_ref().map(|r| r.computed_at),
            Some(NOW + 1000),
            "second result waits for the next modal session"
        );

        // Closing applies the held-back result.
        update(&mut state, key(KeyCode::Esc), NOW + 3000);
        assert_eq!(state.rollout.as_ref().map(|r| r.computed_at), Some(NOW + 2000));

        // Enter commits the rollout-ranked candidate (rows come from picker_rows).
        update(&mut state, key(KeyCode::Char('1')), NOW + 4000);
        let effects = update(&mut state, key(KeyCode::Enter), NOW + 4000);
        assert!(matches!(state.board.setups()[0].status, SetupStatus::Called { .. }));
        assert!(effects.writes.is_empty(), "writes disarmed in this fixture");
    }

    #[test]
    fn setup_freeing_requests_an_immediate_rollout() {
        let mut state = se4_app(false);
        let effects = call_top_candidate(&mut state, '1');
        assert_eq!(effects.sim, Some(super::SimUrgency::Routine), "a call is routine");

        // f frees the setup: the decision-point exemption fires.
        let effects = update(&mut state, key(KeyCode::Char('f')), NOW + 1000);
        assert_eq!(effects.sim, Some(super::SimUrgency::Immediate));

        // A poll applying a snapshot is routine…
        let sets = state.brackets[0].state.sets.clone();
        let effects = update(&mut state, snapshot_msg("ultimate", 1, sets.clone()), NOW + 2000);
        assert_eq!(effects.sim, Some(super::SimUrgency::Routine));

        // …unless its result arrival auto-frees a setup.
        call_top_candidate(&mut state, '1');
        let called = match state.board.setups()[0].status.clone() {
            SetupStatus::Called { set, .. } => set,
            other => panic!("expected Called, got {other:?}"),
        };
        let mut next = sets;
        let ix = next.iter().position(|s| s.key == called).unwrap();
        complete(&mut next[ix], 0, NOW / 1000 + 900);
        let effects = update(&mut state, snapshot_msg("ultimate", 2, next), NOW + 3000);
        assert_eq!(effects.sim, Some(super::SimUrgency::Immediate), "auto-free is a decision point");
    }

    #[test]
    fn reassign_modal_writes_a_persisted_undoable_override() {
        // Two brackets sharing two setups, disjoint players.
        let players_a: Vec<SynthPlayer> = (1..=4)
            .map(|i| SynthPlayer {
                player_id: format!("A{i}"),
                name: format!("Ult {i}"),
            })
            .collect();
        let players_b: Vec<SynthPlayer> = (1..=4)
            .map(|i| SynthPlayer {
                player_id: format!("B{i}"),
                name: format!("Melee {i}"),
            })
            .collect();
        let ultimate = make_de_bracket_with(1001, &players_a);
        let melee = make_de_bracket_with(2001, &players_b);
        let config = test_config(&[1, 2], &["ultimate", "melee"]);
        let boots = bootstrap(vec![("ultimate", &ultimate), ("melee", &melee)]);
        let mut state = AppState::new(config, false, boots, NOW);
        assert!(state.world.queue.iter().any(|e| e.candidate_setups.contains(&SetupId(2))));

        // Select setup 2 (opens the picker on a free setup), then a → the
        // reassign modal for the same setup; dedicate it to melee (option 1).
        update(&mut state, key(KeyCode::Char('2')), NOW);
        update(&mut state, key(KeyCode::Char('a')), NOW);
        assert!(matches!(state.ui.modal, Some(Modal::Reassign { setup: SetupId(2), .. })));
        update(&mut state, key(KeyCode::Down), NOW); // options: [ultimate, melee, any, restore]
        update(&mut state, key(KeyCode::Enter), NOW);

        assert_eq!(
            state.pool_overrides.get(&SetupId(2)),
            Some(&PoolOverride::Dedicated(BracketId("melee".to_owned())))
        );
        // The queue reshaped immediately: ultimate entries lost setup 2.
        assert!(state
            .world
            .queue
            .iter()
            .filter(|e| e.bracket.0 == "ultimate")
            .all(|e| e.candidate_setups == vec![SetupId(1)]));
        // Melee entries kept it (config pool + dedication agree).
        assert!(state
            .world
            .queue
            .iter()
            .filter(|e| e.bracket.0 == "melee")
            .all(|e| e.candidate_setups.contains(&SetupId(2))));

        // The override survives an overlay round trip.
        let doc = state.to_overlay();
        let config = test_config(&[1, 2], &["ultimate", "melee"]);
        let boots = bootstrap(vec![("ultimate", &ultimate), ("melee", &melee)]);
        let mut restored = AppState::new(config, false, boots, NOW);
        restored.apply_overlay(doc, NOW, true);
        assert_eq!(
            restored.pool_overrides.get(&SetupId(2)),
            Some(&PoolOverride::Dedicated(BracketId("melee".to_owned())))
        );

        // Undo reverts it.
        update(&mut state, key(KeyCode::Char('u')), NOW);
        assert!(state.pool_overrides.is_empty());
        assert!(state.world.queue.iter().any(|e| e.candidate_setups.contains(&SetupId(2))));
    }

    #[test]
    fn notices_page_acks_newest_first() {
        let mut state = se4_app(false);
        state.notice(NOW, NoticeLevel::Warn, "older warning");
        state.notice(NOW + 1000, NoticeLevel::Error, "newest error");

        update(&mut state, key(KeyCode::Char('n')), NOW + 2000);
        assert!(matches!(state.ui.modal, Some(Modal::Notices { selected: 0 })));
        // Selected 0 = newest.
        update(&mut state, key(KeyCode::Enter), NOW + 2000);
        assert!(state.notices.iter().any(|n| n.text == "newest error" && n.acked));
        assert!(state.notices.iter().any(|n| n.text == "older warning" && !n.acked));
    }

    #[test]
    fn pending_writes_view_retries_and_discards_parked() {
        let mut state = se4_app(true);
        let effects = call_top_candidate(&mut state, '1');
        let intent = effects.writes[0].clone();
        update(
            &mut state,
            Msg::Write(WriteResult {
                intent: intent.clone(),
                outcome: WriteOutcome::Terminal { error: "500".to_owned() },
            }),
            NOW + 1000,
        );
        assert!(state.pending_writes.iter().any(|p| p.status == PendingStatus::Parked));

        // Enter re-queues the parked write with a fresh attempt budget.
        update(&mut state, key(KeyCode::Char('w')), NOW + 2000);
        let effects = update(&mut state, key(KeyCode::Enter), NOW + 2000);
        assert_eq!(effects.writes, vec![intent.clone()]);
        assert!(state.pending_writes.iter().all(|p| p.status == PendingStatus::Queued));

        // Park it again and discard it (the writes modal is still open).
        update(
            &mut state,
            Msg::Write(WriteResult {
                intent,
                outcome: WriteOutcome::Terminal { error: "500".to_owned() },
            }),
            NOW + 3000,
        );
        assert!(matches!(state.ui.modal, Some(Modal::PendingWrites { .. })));
        update(&mut state, key(KeyCode::Char('d')), NOW + 4000);
        assert!(state.pending_writes.is_empty());
        assert!(state.notices.iter().any(|n| n.text.contains("discarded write")));
    }

    #[test]
    fn unknown_state_int_deviation_is_noticed_and_optionally_escalated() {
        let mut state = se4_app(false);
        let mut next = state.brackets[0].state.sets.clone();
        next[0].state_int = Some(7); // QUEUED — not in the known vocabulary
        update(&mut state, snapshot_msg("ultimate", 1, next.clone()), NOW + 1000);
        assert!(state.notices.iter().any(|n| n.text.contains("unrecognized")));
        assert!(state.soft_busy.is_empty(), "advisory by default");

        let mut escalating = se4_app(false);
        escalating.config.escalate_unpinned_state_deviation = true;
        update(&mut escalating, snapshot_msg("ultimate", 1, next), NOW + 1000);
        assert_eq!(escalating.soft_busy.len(), 1);
    }

    /// Calls the top candidate on setup 1, then opens the report modal for it.
    fn reporting_app() -> AppState {
        let mut state = se4_app(true);
        call_top_candidate(&mut state, '1');
        update(&mut state, key(KeyCode::Char('g')), NOW);
        assert!(matches!(state.ui.modal, Some(Modal::Report(_))), "{:?}", state.ui.modal);
        state
    }

    fn draft(state: &AppState) -> &super::ReportDraft {
        match &state.ui.modal {
            Some(Modal::Report(draft)) => draft,
            other => panic!("expected the report modal, got {other:?}"),
        }
    }

    fn report_payload(effects: &super::UpdateEffects) -> super::ReportPayload {
        effects
            .writes
            .iter()
            .find_map(|w| match &w.kind {
                WriteKind::Report(payload) => Some((**payload).clone()),
                _ => None,
            })
            .expect("a report intent was enqueued")
    }

    #[test]
    fn report_flow_taps_games_and_submits() {
        let mut state = reporting_app();
        let (left_entrant, right_name) = {
            let d = draft(&state);
            (d.left.entrant_id.clone(), d.right.name.clone())
        };

        update(&mut state, key(KeyCode::Char('1')), NOW);
        update(&mut state, key(KeyCode::Char('2')), NOW);
        update(&mut state, key(KeyCode::Char('1')), NOW);
        update(&mut state, key(KeyCode::Enter), NOW); // finish → confirm
        assert!(matches!(draft(&state).stage, super::ReportStage::Confirm { dq: None }));

        let effects = update(&mut state, key(KeyCode::Char('y')), NOW);
        let report = report_payload(&effects);
        assert_eq!(report.winner_entrant_id, Some(left_entrant));
        assert!(!report.is_dq);
        assert_eq!(report.games.len(), 3);
        assert!(report.summary.contains("2-1"), "{}", report.summary);
        assert!(report.summary.contains(&right_name), "{}", report.summary);

        // Submitting frees the station and suppresses stale local evidence.
        assert!(state.ui.modal.is_none());
        assert!(state.board.setups().iter().all(|s| s.status == SetupStatus::Free));
        assert_eq!(state.tombstones.awaiting_remote_completion.len(), 1);
        assert!(effects.force_poll.contains(&BracketId("ultimate".to_owned())));
    }

    #[test]
    fn report_clinch_jumps_to_confirm_and_dq_reports_winner_only() {
        // A known best-of clinches automatically.
        let mut state = reporting_app();
        match &mut state.ui.modal {
            Some(Modal::Report(d)) => d.best_of = Some(3),
            _ => unreachable!(),
        }
        update(&mut state, key(KeyCode::Char('2')), NOW);
        update(&mut state, key(KeyCode::Char('2')), NOW);
        assert!(matches!(draft(&state).stage, super::ReportStage::Confirm { dq: None }));
        // Esc steps back to the game taps instead of losing the draft.
        update(&mut state, key(KeyCode::Esc), NOW);
        assert!(matches!(draft(&state).stage, super::ReportStage::Games));
        assert_eq!(draft(&state).games.len(), 2, "the draft survived");

        // DQ: d, pick the side, confirm — winner is the other side, no games.
        update(&mut state, key(KeyCode::Char('d')), NOW);
        update(&mut state, key(KeyCode::Char('1')), NOW);
        let (left_name, right_entrant) = {
            let d = draft(&state);
            assert!(matches!(
                d.stage,
                super::ReportStage::Confirm {
                    dq: Some(super::Side::Left)
                }
            ));
            (d.left.name.clone(), d.right.entrant_id.clone())
        };
        let effects = update(&mut state, key(KeyCode::Enter), NOW);
        let report = report_payload(&effects);
        assert!(report.is_dq);
        assert_eq!(report.winner_entrant_id, Some(right_entrant));
        assert!(report.games.is_empty(), "DQ reports carry no game data");
        assert!(report.summary.contains("DQ"), "{}", report.summary);
        assert!(report.summary.contains(&left_name), "{}", report.summary);
    }

    #[test]
    fn report_needs_writes_armed() {
        let mut state = se4_app(false);
        call_top_candidate(&mut state, '1');
        update(&mut state, key(KeyCode::Char('g')), NOW);
        assert!(state.ui.modal.is_none());
        assert!(state.notices.iter().any(|n| n.text.contains("advisor-only")));
    }

    #[test]
    fn character_picker_filters_picks_and_sticks() {
        let roster = vec![
            bracket_tools_startgg::CharacterInfo {
                id: 1,
                name: "Mario".to_owned(),
            },
            bracket_tools_startgg::CharacterInfo {
                id: 2,
                name: "Marth".to_owned(),
            },
            bracket_tools_startgg::CharacterInfo {
                id: 3,
                name: "Fox".to_owned(),
            },
        ];
        let mut state = se4_app(true);
        state.brackets[0].characters = roster;
        call_top_candidate(&mut state, '1');
        update(&mut state, key(KeyCode::Char('g')), NOW);

        // Left side: filter "mar", cursor down to Marth, pick.
        update(&mut state, key(KeyCode::Char('c')), NOW);
        for c in "mar".chars() {
            update(&mut state, key(KeyCode::Char(c)), NOW);
        }
        update(&mut state, key(KeyCode::Down), NOW);
        update(&mut state, key(KeyCode::Enter), NOW);
        // Right side: "f" → Fox.
        update(&mut state, key(KeyCode::Char('f')), NOW);
        update(&mut state, key(KeyCode::Enter), NOW);

        let (chars, left_key, right_key) = {
            let d = draft(&state);
            assert!(matches!(d.stage, super::ReportStage::Games));
            (d.chars, d.left.sticky_key.clone(), d.right.sticky_key.clone())
        };
        assert_eq!(chars, [Some(2), Some(3)]);
        assert_eq!(state.last_characters.get(&left_key), Some(&2));
        assert_eq!(state.last_characters.get(&right_key), Some(&3));

        // The picks ride along on every reported game.
        update(&mut state, key(KeyCode::Char('1')), NOW);
        update(&mut state, key(KeyCode::Enter), NOW);
        let effects = update(&mut state, key(KeyCode::Char('y')), NOW);
        let report = report_payload(&effects);
        assert_eq!(report.games.len(), 1);
        let selections = &report.games[0].selections;
        assert_eq!(selections.len(), 2);
        assert_eq!(selections[0].character_id, Some(2));
        assert_eq!(selections[1].character_id, Some(3));

        // Sticky memory survives the overlay round trip.
        let mut restored = se4_app(true);
        restored.apply_overlay(state.to_overlay(), NOW, true);
        assert_eq!(restored.last_characters.get(&left_key), Some(&2));
    }

    #[test]
    fn characters_apply_per_game_and_carry_forward() {
        let roster = vec![
            bracket_tools_startgg::CharacterInfo {
                id: 1,
                name: "Mario".to_owned(),
            },
            bracket_tools_startgg::CharacterInfo {
                id: 2,
                name: "Marth".to_owned(),
            },
            bracket_tools_startgg::CharacterInfo {
                id: 3,
                name: "Fox".to_owned(),
            },
        ];
        let mut state = se4_app(true);
        state.brackets[0].characters = roster;
        call_top_candidate(&mut state, '1');
        update(&mut state, key(KeyCode::Char('g')), NOW);

        // Base picks before any game: Marth / Fox.
        update(&mut state, key(KeyCode::Char('c')), NOW);
        for c in "marth".chars() {
            update(&mut state, key(KeyCode::Char(c)), NOW);
        }
        update(&mut state, key(KeyCode::Enter), NOW);
        update(&mut state, key(KeyCode::Char('f')), NOW);
        update(&mut state, key(KeyCode::Enter), NOW);

        // Two games copy them; then the left player switches to Mario for
        // game 2 onward (cursor already sits on the last recorded game).
        update(&mut state, key(KeyCode::Char('1')), NOW);
        update(&mut state, key(KeyCode::Char('2')), NOW);
        assert_eq!(draft(&state).game_cursor, 1);
        update(&mut state, key(KeyCode::Char('c')), NOW);
        for c in "mario".chars() {
            update(&mut state, key(KeyCode::Char(c)), NOW);
        }
        update(&mut state, key(KeyCode::Enter), NOW);
        update(&mut state, key(KeyCode::Tab), NOW); // right keeps Fox

        // Game 3 inherits the switched pick.
        update(&mut state, key(KeyCode::Char('1')), NOW);
        update(&mut state, key(KeyCode::Enter), NOW);
        let effects = update(&mut state, key(KeyCode::Char('y')), NOW);
        let report = report_payload(&effects);
        assert_eq!(report.games.len(), 3);
        let left_char = |ix: usize| report.games[ix].selections[0].character_id;
        let right_char = |ix: usize| report.games[ix].selections[1].character_id;
        assert_eq!([left_char(0), left_char(1), left_char(2)], [Some(2), Some(1), Some(1)]);
        assert_eq!([right_char(0), right_char(1), right_char(2)], [Some(3), Some(3), Some(3)]);
    }
}

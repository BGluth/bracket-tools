//! The Elm core: [`AppState`], [`Msg`], and the pure synchronous [`update`].
//!
//! `update` performs no I/O and reads no clocks — the caller passes `now` and
//! side effects come back as [`UpdateEffects`] requests (writes to enqueue,
//! events to force-poll). Everything here is driven the same way by the real
//! main loop and by tests.

use std::collections::{HashMap, HashSet, VecDeque};

use bracket_tools_startgg::{SetMutationResult, StartGgId};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Serialize};

use crate::{
    config::{SchedulerConfig, SetupId},
    conflict::{
        callable, state_deviation, AliasMap, BlockReason, BracketView, CallableSet, ConflictIndex, ConflictInputs, ConflictKey,
        PlayerFlags, SetupBoard, SetupStatus, Tombstones, UnixMillis,
    },
    duration::{diff_snapshots, DurationModel},
    model::{BracketId, LiveSet, ModelWarning, PhaseGroupInfo, SetKey, SkippedSet},
    persist::{OverlayDoc, OVERLAY_VERSION},
    ranker::GreedyRanker,
    world::{assigned_sets, recompute, BracketState, World, WorldInputs},
};

/// How long `z` parks a queue entry.
pub const SNOOZE_SECS: i64 = 300;
const NOTICE_CAP: usize = 200;

#[derive(Debug)]
pub enum Msg {
    Key(KeyEvent),
    Poll(PollResult),
    Write(WriteResult),
    /// 1s display tick; also re-runs the recompute so time-gated state
    /// (snoozes, rest windows, bracket open times) stays current.
    Tick,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WriteKind {
    Called,
    InProgress,
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
    Transient {
        error: String,
        attempts: u32,
    },
    /// Given up; parked for the TO to retry or discard.
    Terminal {
        error: String,
    },
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
    },
    Help,
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
    pub pool: Vec<SetupId>,
    pub duration_prior_secs: u64,
    pub prior_weight: f64,
}

/// Side effects `update` wants performed. The main loop translates these
/// into channel sends; tests assert on them directly.
#[derive(Debug, Default)]
pub struct UpdateEffects {
    pub writes: Vec<WriteIntent>,
    /// Events whose next poll should happen immediately (freed setups
    /// awaiting results).
    pub force_poll: Vec<BracketId>,
    pub quit: bool,
}

impl UpdateEffects {
    /// Folds another update's effects in (drain-then-draw coalescing).
    pub fn merge(&mut self, other: Self) {
        self.writes.extend(other.writes);
        self.force_poll.extend(other.force_poll);
        self.quit |= other.quit;
    }
}

/// Undo is single-level and local: it restores the overlay, not writes
/// already handed to the writer.
#[derive(Debug, Clone)]
struct UndoSnapshot {
    board: SetupBoard,
    tombstones: Tombstones,
    flags: PlayerFlags,
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
    pub aliases: AliasMap,
    pub snoozes: HashMap<(BracketId, SetKey), UnixMillis>,
    pub last_completed: HashMap<ConflictKey, UnixMillis>,
    /// When each set first became ready (slots filled) — wait-time credit.
    pub callable_since: HashMap<SetKey, UnixMillis>,
    /// When the TO called each set locally (no-show timer, duration ingest).
    pub called_at: HashMap<(BracketId, SetKey), UnixMillis>,
    pub called_ints: Vec<i32>,
    pub in_progress_ints: Vec<i32>,
    pub soft_busy: Vec<(BracketId, SetKey)>,
    pub durations: DurationModel,
    pub pending_writes: Vec<PendingWrite>,
    pub notices: VecDeque<Notice>,
    /// Latest server-clock offset estimate (from mutation round trips);
    /// retained until a fresher sample replaces it. Not persisted — clocks
    /// drift and a restart re-estimates on the first write.
    pub clock_offset: Option<OffsetSample>,
    no_show_alerted: HashSet<(BracketId, SetKey)>,

    pub world: World,
    pub dirty: bool,
    /// Set whenever a message may have changed the persisted overlay; the main
    /// loop debounces a save and clears it.
    pub overlay_dirty: bool,
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
                        pool: b.pool,
                    },
                    applied_seq: 0,
                    last_good_poll: Some(now_millis),
                    consecutive_failures: 0,
                    health: PollHealth::Ok,
                    suspects: HashSet::new(),
                }
            })
            .collect();

        let mut state = Self {
            board: SetupBoard::new(&config.setups),
            aliases: AliasMap::build(&config.player_aliases),
            called_ints: config.known_called_state_int.into_iter().collect(),
            in_progress_ints: config.known_in_progress_state_int.into_iter().collect(),
            writes_armed,
            brackets,
            flags: PlayerFlags::default(),
            tombstones: Tombstones::default(),
            snoozes: HashMap::new(),
            last_completed: HashMap::new(),
            callable_since: HashMap::new(),
            called_at: HashMap::new(),
            soft_busy: Vec::new(),
            durations,
            pending_writes: Vec::new(),
            notices: VecDeque::new(),
            clock_offset: None,
            no_show_alerted: HashSet::new(),
            world: World::default(),
            dirty: false,
            overlay_dirty: false,
            persist_failed: false,
            ui: UiState::default(),
            undo: None,
            config,
        };
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
            snoozes: flatten_pair_map(&self.snoozes),
            last_completed: self.last_completed.iter().map(|(k, v)| (k.clone(), *v)).collect(),
            callable_since: self.callable_since.iter().map(|(k, v)| (k.clone(), *v)).collect(),
            called_at: flatten_pair_map(&self.called_at),
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
    /// reconciling against the current config (the setup inventory and known
    /// state ints stay config-authoritative) and recomputing the world.
    pub fn apply_overlay(&mut self, doc: OverlayDoc, now_millis: UnixMillis) {
        let mut board = SetupBoard::new(&self.config.setups);
        for setup in doc.board.setups() {
            if self.config.setups.contains(&setup.id) {
                board.set_status(setup.id, setup.status.clone());
            } else {
                self.notice(
                    now_millis,
                    NoticeLevel::Warn,
                    format!("dropped persisted state for setup {} (not in config)", setup.id.0),
                );
            }
        }
        self.board = board;
        self.flags = doc.flags;
        self.tombstones = doc.tombstones;
        self.snoozes = doc.snoozes.into_iter().map(|(b, k, v)| ((b, k), v)).collect();
        self.last_completed = doc.last_completed.into_iter().collect();
        self.callable_since = doc.callable_since.into_iter().collect();
        self.called_at = doc.called_at.into_iter().map(|(b, k, v)| ((b, k), v)).collect();
        // Union so a config pin added since the last run is never dropped.
        merge_ints(&mut self.called_ints, doc.called_ints);
        merge_ints(&mut self.in_progress_ints, doc.in_progress_ints);
        self.soft_busy = doc.soft_busy;
        self.durations.restore(doc.durations);
        // A write left in flight when we crashed has an uncertain fate; park it
        // for the TO rather than silently re-sending (avoids a duplicate) or
        // leaving a Queued entry that suppresses a fresh enqueue.
        self.pending_writes = doc
            .pending_writes
            .into_iter()
            .map(|mut p| {
                if p.status == PendingStatus::Queued {
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
    // desk never churns saves.
    match msg {
        Msg::Key(key) => {
            handle_key(state, key, now_millis, &mut effects);
            state.overlay_dirty = true;
        }
        Msg::Poll(poll) => {
            handle_poll(state, poll, now_millis, &mut effects);
            state.overlay_dirty = true;
        }
        Msg::Write(result) => {
            handle_write_result(state, result, now_millis);
            state.overlay_dirty = true;
        }
        Msg::Tick => {
            scan_no_shows(state, now_millis);
            state.dirty = true;
        }
    }
    if state.dirty {
        state.world = recompute_world(state, now_millis);
        state.ui.queue_ix = state.ui.queue_ix.min(state.world.queue.len().saturating_sub(1));
        state.dirty = false;
    }
    effects
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
        KeyCode::Char(c @ '0'..='9') => select_setup(state, c, now),
        KeyCode::Char('p') => progress_selected(state, now, effects),
        KeyCode::Char('f') => free_selected(state, now, effects),
        KeyCode::Char('r') => requeue_selected(state, now),
        KeyCode::Char('z') => snooze_selected(state, now),
        KeyCode::Char('u') => undo(state, now),
        KeyCode::Up => state.ui.queue_ix = state.ui.queue_ix.saturating_sub(1),
        KeyCode::Down => {
            state.ui.queue_ix = (state.ui.queue_ix + 1).min(state.world.queue.len().saturating_sub(1));
        }
        _ => {}
    }
}

fn handle_modal_key(state: &mut AppState, key: KeyEvent, now: UnixMillis, effects: &mut UpdateEffects) {
    if key.code == KeyCode::Esc {
        state.ui.modal = None;
        return;
    }
    let Some(Modal::CallPicker { setup, selected }) = state.ui.modal.clone() else {
        // Help modal: any other key closes it too.
        state.ui.modal = None;
        return;
    };
    let candidates = state.world.per_setup.get(&setup).map_or(0, Vec::len);
    match key.code {
        KeyCode::Up => {
            state.ui.modal = Some(Modal::CallPicker {
                setup,
                selected: selected.saturating_sub(1),
            });
        }
        KeyCode::Down => {
            state.ui.modal = Some(Modal::CallPicker {
                setup,
                selected: (selected + 1).min(candidates.saturating_sub(1)),
            });
        }
        KeyCode::Enter => commit_call(state, setup, selected, now, effects),
        _ => {}
    }
}

/// Digits map straight to the TO's setup numbering (`SetupId(d)`, `0` = 10).
fn select_setup(state: &mut AppState, digit: char, now: UnixMillis) {
    let number = digit.to_digit(10).map(|d| if d == 0 { 10 } else { d }).unwrap_or(0);
    let setup = SetupId(number);
    let Some(status) = state.board.setups().iter().find(|s| s.id == setup).map(|s| s.status.clone()) else {
        state.notice(now, NoticeLevel::Warn, format!("no setup {number} configured"));
        return;
    };
    match status {
        SetupStatus::Free => {
            state.ui.selected_setup = Some(setup);
            state.ui.modal = Some(Modal::CallPicker { setup, selected: 0 });
        }
        _ => {
            state.ui.selected_setup = Some(setup);
        }
    }
}

fn commit_call(state: &mut AppState, setup: SetupId, selected: usize, now: UnixMillis, effects: &mut UpdateEffects) {
    state.ui.modal = None;
    let Some(entry) = state.world.per_setup.get(&setup).and_then(|list| list.get(selected)).cloned() else {
        state.notice(now, NoticeLevel::Warn, "no candidate selected");
        return;
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
    state.dirty = true;
    state.notice(now, NoticeLevel::Info, format!("setup {} freed, awaiting result", setup.0));
}

fn requeue_selected(state: &mut AppState, now: UnixMillis) {
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
    if state
        .pending_writes
        .iter()
        .any(|p| p.intent.id == id && p.intent.kind == kind && p.status == PendingStatus::Queued)
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
    let views: Vec<BracketView<'_>> = state
        .brackets
        .iter()
        .map(|b| BracketView {
            id: &b.state.id,
            sets: &b.state.sets,
            mode: b.state.mode,
            start_at: b.state.start_at,
            held: b.state.held,
            pool: &b.state.pool,
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

fn handle_poll(state: &mut AppState, poll: PollResult, now: UnixMillis, _effects: &mut UpdateEffects) {
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
            apply_snapshot(state, ix, poll.seq, poll.captured_at, sets, now);
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

fn apply_snapshot(state: &mut AppState, ix: usize, seq: u64, captured_at: UnixMillis, mut sets: Vec<LiveSet>, now: UnixMillis) {
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
        free_setups_holding(state, &pair, now);
    }

    ingest_deviations(state, ix, &prev, now);
    stamp_ready(state, ix, now);
    state.dirty = true;
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
fn free_setups_holding(state: &mut AppState, pair: &(BracketId, SetKey), now: UnixMillis) {
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
    for setup in held {
        state.board.set_status(setup, SetupStatus::Free);
        state.notice(now, NoticeLevel::Info, format!("setup {} free (result arrived)", setup.0));
    }
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
                learn_state_int(state, intent.kind, new_int, now);
            }
            if offset.is_some() {
                state.clock_offset = offset;
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
        WriteOutcome::Terminal { error } => {
            if let Some(pending) = state.pending_writes.iter_mut().find(|p| p.intent == intent) {
                pending.status = PendingStatus::Parked;
                pending.attempts += 1;
                pending.last_error = Some(error.clone());
            }
            let text = format!("write {:?} for set {} failed for good: {error}", intent.kind, intent.id);
            state.notice(now, NoticeLevel::Error, text);
        }
    }
}

fn learn_state_int(state: &mut AppState, kind: WriteKind, new_int: i32, now: UnixMillis) {
    let learned = match kind {
        WriteKind::Called => &mut state.called_ints,
        WriteKind::InProgress => &mut state.in_progress_ints,
    };
    if !learned.contains(&new_int) {
        learned.push(new_int);
        let text = format!("learned {kind:?} state int = {new_int}");
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
    use bracket_tools_startgg::SetMutationResult;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::{
        update, AppState, BracketBootstrap, Modal, Msg, NoticeLevel, PendingStatus, PollFailure, PollOutcome, PollResult, WriteIntent,
        WriteKind, WriteOutcome, WriteResult,
    };
    use crate::{
        config::{BracketConfig, BracketMode, SchedulerConfig, SetupId},
        conflict::{BlockReason, SetupStatus},
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
            setups: setups.iter().map(|&n| SetupId(n)).collect(),
            brackets: brackets
                .iter()
                .map(|slug| BracketConfig {
                    pool: setups.iter().map(|&n| SetupId(n)).collect(),
                    ..BracketConfig::new(*slug)
                })
                .collect(),
            known_called_state_int: Some(6),
            known_in_progress_state_int: Some(2),
            ..SchedulerConfig::default()
        }
    }

    fn bootstrap(config: &SchedulerConfig, brackets: Vec<(&str, &SynthBracket)>) -> Vec<BracketBootstrap> {
        brackets
            .into_iter()
            .map(|(slug, bracket)| BracketBootstrap {
                id: BracketId(slug.to_owned()),
                sets: bracket.sets.clone(),
                groups: vec![bracket.info.clone()],
                mode: BracketMode::Full,
                start_at: None,
                pool: config.setups.clone(),
                duration_prior_secs: 480,
                prior_weight: 4.0,
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
        let boots = bootstrap(&config, vec![("ultimate", &se4())]);
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
        let boots = bootstrap(&config, vec![("ultimate", &ultimate), ("melee", &melee)]);
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
        let boots = bootstrap(&config, vec![("ultimate", &ultimate), ("melee", &melee)]);
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
        // Same instant, repeated recomputes: byte-identical queues.
        let mut state = se4_app(false);
        update(&mut state, Msg::Tick, NOW + 1000);
        let first = state.world.queue.clone();
        update(&mut state, Msg::Tick, NOW + 1000);
        assert_eq!(first, state.world.queue);

        // Later instant: only the wait-time credit may move; the ordering
        // identity must not.
        update(&mut state, Msg::Tick, NOW + 60_000);
        let order = |entries: &[crate::world::QueueEntry]| entries.iter().map(|e| e.key.clone()).collect::<Vec<_>>();
        assert_eq!(order(&first), order(&state.world.queue));
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

        // Just before expiry: still hidden. After: back.
        update(&mut state, Msg::Tick, NOW + (super::SNOOZE_SECS - 1) * 1000);
        assert_eq!(state.world.queue.len(), 1);
        update(&mut state, Msg::Tick, NOW + (super::SNOOZE_SECS + 1) * 1000);
        assert_eq!(state.world.queue.len(), 2);
    }

    #[test]
    fn preview_ids_never_enqueue_writes() {
        let config = test_config(&[1], &["ultimate"]);
        let preview = make_se_bracket(1001, 4); // preview_* ids
        let boots = bootstrap(&config, vec![("ultimate", &preview)]);
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
            pool: vec![SetupId(1), SetupId(2)],
            duration_prior_secs: 480,
            prior_weight: 4.0,
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
        assert!(state.brackets[0].state.sets.iter().any(|s| s.key == victim), "first absence: suspect");
        update(&mut state, snapshot_msg("ultimate", 4, torn), NOW + 120_000);
        assert!(
            !state.brackets[0].state.sets.iter().any(|s| s.key == victim),
            "second absence: gone"
        );
        assert!(state.notices.iter().any(|n| n.text.contains("removed server-side")));
        assert_eq!(state.world.queue.len(), 1);
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
        restored.apply_overlay(doc, NOW);

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
}

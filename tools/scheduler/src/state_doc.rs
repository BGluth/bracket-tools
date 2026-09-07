//! The persisted state's serialized shape: the operator overlay and the
//! last-good snapshot as versioned, plain-JSON documents. Maps with non-string
//! keys become vectors of pairs; `persist` writes and reads these files.

use serde::{Deserialize, Serialize};

use crate::{
    app::{Notice, PendingWrite},
    config::SetupId,
    conflict::{ConflictKey, PlayerFlags, PoolOverride, SetupBoard, Tombstones, UnixMillis},
    duration::DurationModel,
    model::{BracketId, LiveSet, PhaseGroupInfo, SetKey},
};

/// Bumped when the on-disk shape changes incompatibly; an older file then
/// recovers to `.bak` rather than mis-parsing.
pub const OVERLAY_VERSION: u32 = 1;
pub const SNAPSHOT_VERSION: u32 = 1;

/// The persisted overlay. Maps with non-string keys become vectors of pairs so
/// the document is plain JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverlayDoc {
    pub version: u32,
    pub board: SetupBoard,
    pub flags: PlayerFlags,
    pub tombstones: Tombstones,
    #[serde(default)]
    pub pool_overrides: Vec<(SetupId, PoolOverride)>,
    pub snoozes: Vec<(BracketId, SetKey, UnixMillis)>,
    pub last_completed: Vec<(ConflictKey, UnixMillis)>,
    pub callable_since: Vec<(SetKey, UnixMillis)>,
    pub called_at: Vec<(BracketId, SetKey, UnixMillis)>,
    /// Sticky character memory (player key -> character id); defaulted so
    /// pre-reporting overlays still load.
    #[serde(default)]
    pub last_characters: Vec<(String, i32)>,
    /// Display toggle (`t`): sponsor prefixes hidden. Serde default so
    /// pre-existing overlays load.
    #[serde(default)]
    pub hide_sponsors: bool,
    pub called_ints: Vec<i32>,
    pub in_progress_ints: Vec<i32>,
    pub soft_busy: Vec<(BracketId, SetKey)>,
    pub durations: DurationModel,
    pub pending_writes: Vec<PendingWrite>,
    /// Unacked, correctness-relevant notices only (see `AppState::to_overlay`).
    pub notices: Vec<Notice>,
    pub no_show_alerted: Vec<(BracketId, SetKey)>,
}

/// The last good per-event set tables: the offline cold-start seed. Remote
/// state authority is untouched — this is a stale cache with a visible age,
/// not owned state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotDoc {
    pub version: u32,
    pub brackets: Vec<BracketSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BracketSnapshot {
    pub id: BracketId,
    /// When this table was captured (unix millis) — restart staleness age.
    pub captured_at: UnixMillis,
    pub sets: Vec<LiveSet>,
    pub groups: Vec<PhaseGroupInfo>,
}

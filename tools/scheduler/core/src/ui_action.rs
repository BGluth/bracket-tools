//! Semantic UI intents: what the desk wants done, independent of how it was
//! asked. The keymap produces them from keys; `app::update` applies them.

use crate::{
    config::SetupId,
    conflict::UnixMillis,
    model::{BracketId, SetKey},
};

/// Cursor movement over whichever list has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Move {
    Up,
    Down,
    PageUp,
    PageDown,
}

/// One side of a reported set.
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

    pub fn other(self) -> Side {
        match self {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
        }
    }
}

/// One intent with its explicit target. Index-addressed variants are for
/// the keymap, where the row was resolved in the same turn; a shell round-trip
/// uses the identity-keyed ones, which survive a re-ranked queue or picker.
#[derive(Debug, Clone, PartialEq)]
pub enum UiAction {
    Quit,
    Undo,
    ToggleSponsors,
    MoveQueueCursor(Move),
    MoveModalCursor(Move),
    CloseModal,
    OpenHelp,
    OpenInspection,
    OpenNotices,
    OpenPendingWrites,
    OpenSetups,
    OpenFindSet,
    /// Player flags for the queue entry at this index.
    OpenFlags(usize),
    OpenReassign(SetupId),
    OpenReport(SetupId),
    /// Focus a setup; a free one opens its call picker.
    SelectSetup(SetupId),
    /// Digits typed toward a setup number on a >10-station board, waiting on
    /// the next key or the grace tick.
    BufferSetupDigits(String),
    ClearSetupEntry,
    /// Call the picker's `candidate` row onto `setup`.
    Call {
        setup: SetupId,
        candidate: usize,
    },
    /// Call the queue entry at this index onto its first free candidate setup.
    QuickCall(usize),
    Progress(SetupId),
    Free(SetupId),
    Requeue(SetupId),
    /// Park the queue entry at this index.
    Snooze(usize),
    AckNotice(usize),
    ClearNotices,
    RetryParked(usize),
    DiscardPending(usize),
    CycleFlag(usize),
    ApplyReassign {
        setup: SetupId,
        option: usize,
    },
    SetFindQuery(String),
    OpenFoundSet(usize),
    SetSetupsCountEntry(String),
    ApplySetupsRow(usize),
    ApplySetupsCount {
        row: usize,
        target: u32,
    },
    Report(ReportAction),
    /// Call one of the picker's sets onto `setup`.
    CallSet {
        setup: SetupId,
        bracket: BracketId,
        key: SetKey,
    },
    /// Call a queued set onto its first free candidate setup.
    QuickCallSet {
        bracket: BracketId,
        key: SetKey,
    },
    SnoozeSet {
        bracket: BracketId,
        key: SetKey,
    },
    OpenFlagsFor {
        bracket: BracketId,
        key: SetKey,
    },
    /// Ack the notice posted at this instant.
    AckNoticeAt(UnixMillis),
}

/// Intents inside the report modal.
#[derive(Debug, Clone, PartialEq)]
pub enum ReportAction {
    RecordGame(Side),
    UndoGame,
    MoveGameCursor(Move),
    OpenCharacterPicker,
    StartDq,
    FinishGames,
    SetCharacterFilter(String),
    MoveCharacterCursor(Move),
    /// Commit the character picker for the current side; `None` keeps
    /// whatever the side already had.
    PickCharacter(Option<i32>),
    /// Step back to the game taps.
    Back,
    PickDq(Side),
    Submit,
}

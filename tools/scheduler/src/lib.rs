use cfg_if::cfg_if;

pub mod app;
pub mod config;
pub mod conflict;
pub mod duration;
pub mod fixture_source;
pub mod graph;
pub mod init;
pub mod keymap;
pub mod model;
pub mod poller;
pub mod preflight;
pub mod ranker;
pub mod rehearsal;
pub mod rollout;
pub mod roster_cache;
pub mod set_source;
pub mod simulator;
pub mod state_doc;
#[doc(hidden)]
pub mod synth;
pub mod timers;
pub mod ui_action;
pub mod world;
pub mod writer;

// Terminal and filesystem shells. The modules above are the wasm32-clean
// core (guarded by scripts/wasm-check.sh).
cfg_if! {
    if #[cfg(not(target_arch = "wasm32"))] {
        pub mod cli;
        pub mod persist;
        pub mod replay;
        pub mod terminal;
        pub mod ui;
    }
}

pub use config::{BracketConfig, BracketMode, ConfigError, SchedulerConfig, SetupId};
pub use conflict::{
    callable, callable_sets, BlockReason, BracketView, CallableSet, ConflictIndex, ConflictInputs, ConflictKey, SetupBoard, SetupStatus,
};
pub use duration::{diff_snapshots, DurationModel, SnapshotDiff};
pub use graph::{BracketGraph, GraphWarning};
pub use model::{BracketId, LiveSet, PhaseGroupInfo, SetId, SetKey};
pub use ranker::{GreedyRanker, RankedAction, RankedCandidate, Ranker};
pub use rollout::RolloutRanker;
pub use simulator::{
    simulate, simulate_action, simulate_autoplay, simulate_recorded, Action, ReplayEvent, RunnerUp, ScriptFrame, SetContext, SimBracket,
    SimOutcome, SimWorld,
};

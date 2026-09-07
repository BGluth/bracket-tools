use cfg_if::cfg_if;

pub mod config;
pub mod conflict;
pub mod duration;
pub mod graph;
pub mod init;
pub mod model;
pub mod ranker;
pub mod rollout;
pub mod set_source;
pub mod simulator;
#[doc(hidden)]
pub mod synth;
pub mod ui_action;
pub mod world;

// Terminal, filesystem and tokio-runtime shells, plus the Elm core and its
// keymap, which reach the persistence documents. The modules above are the
// wasm32-clean core (guarded by scripts/wasm-check.sh).
cfg_if! {
    if #[cfg(not(target_arch = "wasm32"))] {
        pub mod app;
        pub mod cli;
        pub mod fixture_source;
        pub mod keymap;
        pub mod persist;
        pub mod poller;
        pub mod preflight;
        pub mod rehearsal;
        pub mod replay;
        pub mod roster_cache;
        pub mod terminal;
        pub mod ui;
        pub mod writer;
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

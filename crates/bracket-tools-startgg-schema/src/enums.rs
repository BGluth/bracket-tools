use crate::schema::schema;

/// Lifecycle state shared by start.gg activities (events, phases, etc.).
///
/// Note this is distinct from `Set.state` / `PhaseGroup.state`, which the SDL
/// types as a bare undocumented `Int`.
#[derive(cynic::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivityState {
    Created,
    Active,
    Completed,
    Ready,
    Invalid,
    Called,
    Queued,
}

#[derive(cynic::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum BracketType {
    SingleElimination,
    DoubleElimination,
    RoundRobin,
    Swiss,
    Exhibition,
    CustomSchedule,
    Matchmaking,
    EliminationRounds,
    Race,
    Circuit,
}

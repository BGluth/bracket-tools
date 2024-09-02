use normalized_data::hydrated_normalized_data::{
    NormalizedBracket, NormalizedGame, NormalizedPlayer, NormalizedPlayerGameInfo, NormalizedSet, NormalizedTournament,
};

pub type TournamentId = u64;
pub type EventId = u64;
pub type SetId = u64;
pub type GameId = u64;
pub type PlayerId = u64;

// For now (will very likely change), the gg core types will mirror the normalized types.
pub type GgTournament = NormalizedTournament;
pub type GgBracket = NormalizedBracket;
pub type GgSet = NormalizedSet;
pub type GgGame = NormalizedGame;
pub type GgPlayerGameInfo = NormalizedPlayerGameInfo;
pub type GgPlayer = NormalizedPlayer;

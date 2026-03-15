use bracket_tools_core::data_types::Dehydrateable;
use serde::{Deserialize, Serialize};

pub type StartGgId = u64;

// GG-layer ID aliases (used by provider stubs)
pub type TournamentId = u64;
pub type EventId = u64;
pub type SetId = u64;
pub type GameId = u64;
pub type PlayerId = u64;

// Dehydrated newtypes (ID-only references)

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DehydratedGgTournament(pub StartGgId);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DehydratedGgBracket(pub StartGgId);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DehydratedGgSet(pub StartGgId);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DehydratedGgGame(pub StartGgId);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DehydratedGgPlayer(pub StartGgId);

// Hydrated types (full data from API responses)

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HydratedGgTournament {
    pub id: StartGgId,
    pub name: String,
    pub participant_ids: Vec<StartGgId>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HydratedGgBracket {
    pub id: StartGgId,
}

/// Per-slot data extracted from a start.gg set response.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SlotData {
    pub entrant_id: StartGgId,
    pub player_id: StartGgId,
    pub score: Option<f64>,
}

/// The format of a set, encoding the number of players per side.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Matchup {
    Singles { left: SlotData, right: SlotData },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HydratedGgSet {
    pub id: StartGgId,
    pub completed_at: Option<String>,
    pub round: Option<i32>,
    pub matchup: Option<Matchup>,
    pub games: Vec<HydratedGgGame>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HydratedGgGame {
    pub id: Option<StartGgId>,
    pub winner_id: Option<StartGgId>,
    pub selections: Vec<GgCharacterSelection>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HydratedGgPlayer {
    pub id: StartGgId,
    pub gamer_tag: String,
    pub prefix: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GgCharacterSelection {
    pub character_id: Option<StartGgId>,
}

// Convenience aliases
pub type GgTournament = HydratedGgTournament;
pub type GgBracket = HydratedGgBracket;
pub type GgSet = HydratedGgSet;
pub type GgGame = HydratedGgGame;
pub type GgPlayer = HydratedGgPlayer;

// Dehydrateable impls

impl Dehydrateable for HydratedGgTournament {
    type Dehydrated = DehydratedGgTournament;

    fn dehydrate(&self) -> DehydratedGgTournament {
        DehydratedGgTournament(self.id)
    }
}

impl Dehydrateable for HydratedGgBracket {
    type Dehydrated = DehydratedGgBracket;

    fn dehydrate(&self) -> DehydratedGgBracket {
        DehydratedGgBracket(self.id)
    }
}

impl Dehydrateable for HydratedGgSet {
    type Dehydrated = DehydratedGgSet;

    fn dehydrate(&self) -> DehydratedGgSet {
        DehydratedGgSet(self.id)
    }
}

impl Dehydrateable for HydratedGgGame {
    type Dehydrated = Option<DehydratedGgGame>;

    fn dehydrate(&self) -> Option<DehydratedGgGame> {
        self.id.map(DehydratedGgGame)
    }
}

impl Dehydrateable for HydratedGgPlayer {
    type Dehydrated = DehydratedGgPlayer;

    fn dehydrate(&self) -> DehydratedGgPlayer {
        DehydratedGgPlayer(self.id)
    }
}

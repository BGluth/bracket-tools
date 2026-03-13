pub type StartGgId = u64;

pub type TournamentId = u64;
pub type EventId = u64;
pub type SetId = u64;
pub type GameId = u64;
pub type PlayerId = u64;

pub type GgTournament = HydratedGgTournament;
pub type GgBracket = HydratedGgBracket;
pub type GgSet = HydratedGgSet;
pub type GgGame = HydratedGgGame;
pub type GgPlayer = HydratedGgPlayer;

#[derive(Clone, Debug)]
pub struct DehydratedGgTournament(pub StartGgId);

#[derive(Clone, Debug)]
pub struct DehydratedGgBracket(pub StartGgId);

#[derive(Clone, Debug)]
pub struct DehydratedGgSet(pub StartGgId);

#[derive(Clone, Debug)]
pub struct DehydratedGgGame(pub StartGgId);

#[derive(Clone, Debug)]
pub struct DehydratedGgPlayer(pub StartGgId);

#[derive(Clone, Debug)]
pub struct HydratedGgTournament {
    pub name: String,
    pub participants: Vec<GgPlayer>,
}

#[derive(Clone, Debug)]
pub struct HydratedGgBracket {}

#[derive(Clone, Debug)]
pub struct HydratedGgSet {}

#[derive(Clone, Debug)]
pub struct HydratedGgGame {}

#[derive(Clone, Debug)]
pub struct HydratedGgPlayer {}

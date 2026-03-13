use serde::{Deserialize, Serialize};

pub type Id = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TournamentId(pub Id);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BracketId(pub Id);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SetId(pub Id);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GameId(pub Id);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PlayerId(pub Id);

// TODO: No idea how to represent this data. This needs to be game agnostic, so
// this might be a bit tricky...
pub type PlayerGameMetaInfo = u64;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Tournament {
    pub t_id: TournamentId,
    pub name: String,
    pub brackets: Vec<Bracket>,
    pub admins: Vec<AdminAndPrivilegeLevel>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AdminAndPrivilegeLevel {
    pub p_id: PlayerId,
    pub p_level: AdminPrivilegeLevel,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum AdminPrivilegeLevel {}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bracket {
    pub b_id: BracketId,
    pub b_type: BracketType,
    pub sets: Vec<Set>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum BracketType {
    DoubleElim,
    RoundRobin,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Set {
    pub s_id: SetId,
    pub games: Vec<Game>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Game {
    pub g_id: GameId,
    pub g_type: GameType,
    pub winning_side: GameWinningSide,
}

/// Kind of a weird way to do this, but we need a bit to indicate which side won
/// in a game. Might need to make this more complicated down the road for when
/// we support more games.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum GameWinningSide {
    Left = 0,
    Right = 1,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum GameType {
    OneVOne(PlayerGameInfo, PlayerGameInfo),
    MultiVsMulti(Vec<PlayerGameInfo>, Vec<PlayerGameInfo>),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlayerGameInfo {
    pub p_id: PlayerId,
    pub meta: PlayerGameMetaInfo,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Player {
    pub p_id: PlayerId,
    pub name: String,
    pub prefix: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip<T: Serialize + for<'de> Deserialize<'de> + std::fmt::Debug>(val: &T) {
        let json = serde_json::to_string(val).expect("serialize");
        let _: T = serde_json::from_str(&json).expect("deserialize");
    }

    #[test]
    fn id_newtypes_round_trip() {
        round_trip(&TournamentId(1));
        round_trip(&BracketId(2));
        round_trip(&SetId(3));
        round_trip(&GameId(4));
        round_trip(&PlayerId(5));
    }

    #[test]
    fn player_round_trip() {
        round_trip(&Player {
            p_id: PlayerId(42),
            name: "Tweek".into(),
            prefix: "TSM".into(),
        });
    }

    #[test]
    fn game_one_v_one_round_trip() {
        round_trip(&Game {
            g_id: GameId(1),
            g_type: GameType::OneVOne(
                PlayerGameInfo { p_id: PlayerId(1), meta: 0 },
                PlayerGameInfo { p_id: PlayerId(2), meta: 0 },
            ),
            winning_side: GameWinningSide::Left,
        });
    }

    #[test]
    fn game_multi_vs_multi_round_trip() {
        round_trip(&Game {
            g_id: GameId(2),
            g_type: GameType::MultiVsMulti(
                vec![PlayerGameInfo { p_id: PlayerId(1), meta: 0 }],
                vec![PlayerGameInfo { p_id: PlayerId(2), meta: 0 }],
            ),
            winning_side: GameWinningSide::Right,
        });
    }

    #[test]
    fn set_round_trip() {
        round_trip(&Set {
            s_id: SetId(10),
            games: vec![Game {
                g_id: GameId(1),
                g_type: GameType::OneVOne(
                    PlayerGameInfo { p_id: PlayerId(1), meta: 0 },
                    PlayerGameInfo { p_id: PlayerId(2), meta: 0 },
                ),
                winning_side: GameWinningSide::Left,
            }],
        });
    }

    #[test]
    fn bracket_type_round_trip() {
        round_trip(&BracketType::DoubleElim);
        round_trip(&BracketType::RoundRobin);
    }

    #[test]
    fn bracket_round_trip() {
        round_trip(&Bracket {
            b_id: BracketId(5),
            b_type: BracketType::DoubleElim,
            sets: vec![],
        });
    }

    #[test]
    fn tournament_round_trip() {
        round_trip(&Tournament {
            t_id: TournamentId(100),
            name: "Don't Park on the Grass".into(),
            brackets: vec![],
            admins: vec![],
        });
    }
}

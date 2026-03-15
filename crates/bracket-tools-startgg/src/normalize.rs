use bracket_tools_core::{
    data_types::Normalizable,
    types::{
        Game, GameId, GameType, GameWinningSide, Player, PlayerId, PlayerGameInfo, Set, SetId,
        Tournament, TournamentId,
    },
};

use crate::gg_data_types::{
    HydratedGgGame, HydratedGgPlayer, HydratedGgSet, HydratedGgTournament, Matchup, SlotData,
    StartGgId,
};

/// Pairs a game with slot context needed to resolve `winner_id` to a side.
pub struct GgGameWithContext<'a> {
    pub game: &'a HydratedGgGame,
    pub matchup: &'a Matchup,
}

impl Normalizable for HydratedGgPlayer {
    type NormalizedData = Player;

    fn normalize(&self) -> Player {
        Player {
            p_id: PlayerId(self.id),
            name: self.gamer_tag.clone(),
            prefix: self.prefix.clone().unwrap_or_default(),
        }
    }
}

impl<'a> Normalizable for GgGameWithContext<'a> {
    type NormalizedData = Option<Game>;

    fn normalize(&self) -> Option<Game> {
        let winner_id = self.game.winner_id?;

        match self.matchup {
            Matchup::Singles { left, right } => {
                let winning_side = determine_winning_side(winner_id, left, right)?;

                Some(Game {
                    g_id: GameId(self.game.id.unwrap_or(0)),
                    g_type: GameType::OneVOne(
                        PlayerGameInfo {
                            p_id: PlayerId(left.player_id),
                            meta: 0,
                        },
                        PlayerGameInfo {
                            p_id: PlayerId(right.player_id),
                            meta: 0,
                        },
                    ),
                    winning_side,
                })
            }
        }
    }
}

fn determine_winning_side(
    winner_id: StartGgId,
    left: &SlotData,
    right: &SlotData,
) -> Option<GameWinningSide> {
    if left.entrant_id == winner_id {
        Some(GameWinningSide::Left)
    } else if right.entrant_id == winner_id {
        Some(GameWinningSide::Right)
    } else {
        None
    }
}

impl Normalizable for HydratedGgSet {
    type NormalizedData = Set;

    fn normalize(&self) -> Set {
        let games = match &self.matchup {
            Some(matchup) => self
                .games
                .iter()
                .filter_map(|game| {
                    GgGameWithContext { game, matchup }.normalize()
                })
                .collect(),
            None => vec![],
        };

        Set {
            s_id: SetId(self.id),
            games,
        }
    }
}

impl Normalizable for HydratedGgTournament {
    type NormalizedData = Tournament;

    fn normalize(&self) -> Tournament {
        Tournament {
            t_id: TournamentId(self.id),
            name: self.name.clone(),
            brackets: vec![],
            admins: vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{GgGameWithContext, Normalizable};
    use crate::gg_data_types::{
        HydratedGgGame, HydratedGgPlayer, HydratedGgSet, HydratedGgTournament, Matchup, SlotData,
    };
    use bracket_tools_core::types::{GameType, GameWinningSide, PlayerId, SetId, TournamentId};

    fn singles_matchup(
        left_entrant: u64,
        left_player: u64,
        right_entrant: u64,
        right_player: u64,
    ) -> Matchup {
        Matchup::Singles {
            left: SlotData {
                entrant_id: left_entrant,
                player_id: left_player,
                score: None,
            },
            right: SlotData {
                entrant_id: right_entrant,
                player_id: right_player,
                score: None,
            },
        }
    }

    #[test]
    fn player_normalizes_with_prefix() {
        let player = HydratedGgPlayer {
            id: 42,
            gamer_tag: "Tweek".to_string(),
            prefix: Some("TSM".to_string()),
        };
        let normalized = player.normalize();

        assert_eq!(normalized.p_id, PlayerId(42));
        assert_eq!(normalized.name, "Tweek");
        assert_eq!(normalized.prefix, "TSM");
    }

    #[test]
    fn player_normalizes_without_prefix() {
        let player = HydratedGgPlayer {
            id: 1,
            gamer_tag: "MkLeo".to_string(),
            prefix: None,
        };
        let normalized = player.normalize();

        assert_eq!(normalized.prefix, "");
    }

    #[test]
    fn game_winner_left() {
        let matchup = singles_matchup(100, 10, 200, 20);
        let game = HydratedGgGame {
            id: Some(1),
            winner_id: Some(100),
            selections: vec![],
        };
        let ctx = GgGameWithContext { game: &game, matchup: &matchup };

        let normalized = ctx.normalize().unwrap();
        assert!(matches!(normalized.winning_side, GameWinningSide::Left));
        // Player IDs (not entrant IDs) should populate PlayerId
        match &normalized.g_type {
            GameType::OneVOne(left, right) => {
                assert_eq!(left.p_id, PlayerId(10));
                assert_eq!(right.p_id, PlayerId(20));
            }
            _ => panic!("expected OneVOne"),
        }
    }

    #[test]
    fn game_winner_right() {
        let matchup = singles_matchup(100, 10, 200, 20);
        let game = HydratedGgGame {
            id: Some(1),
            winner_id: Some(200),
            selections: vec![],
        };
        let ctx = GgGameWithContext { game: &game, matchup: &matchup };

        let normalized = ctx.normalize().unwrap();
        assert!(matches!(normalized.winning_side, GameWinningSide::Right));
    }

    #[test]
    fn game_unknown_winner_returns_none() {
        let matchup = singles_matchup(100, 10, 200, 20);
        let game = HydratedGgGame {
            id: Some(1),
            winner_id: Some(999),
            selections: vec![],
        };
        let ctx = GgGameWithContext { game: &game, matchup: &matchup };

        assert!(ctx.normalize().is_none());
    }

    #[test]
    fn game_no_winner_returns_none() {
        let matchup = singles_matchup(100, 10, 200, 20);
        let game = HydratedGgGame {
            id: Some(1),
            winner_id: None,
            selections: vec![],
        };
        let ctx = GgGameWithContext { game: &game, matchup: &matchup };

        assert!(ctx.normalize().is_none());
    }

    #[test]
    fn set_normalization_filters_unresolvable_games() {
        let set = HydratedGgSet {
            id: 50,
            completed_at: None,
            round: Some(1),
            matchup: Some(singles_matchup(100, 10, 200, 20)),
            games: vec![
                HydratedGgGame {
                    id: Some(1),
                    winner_id: Some(100),
                    selections: vec![],
                },
                HydratedGgGame {
                    id: Some(2),
                    winner_id: Some(999), // unresolvable
                    selections: vec![],
                },
                HydratedGgGame {
                    id: Some(3),
                    winner_id: Some(200),
                    selections: vec![],
                },
            ],
        };
        let normalized = set.normalize();

        assert_eq!(normalized.s_id, SetId(50));
        assert_eq!(normalized.games.len(), 2);
        assert!(matches!(
            normalized.games[0].winning_side,
            GameWinningSide::Left
        ));
        assert!(matches!(
            normalized.games[1].winning_side,
            GameWinningSide::Right
        ));
    }

    #[test]
    fn tournament_normalization() {
        let tournament = HydratedGgTournament {
            id: 100,
            name: "Don't Park on the Grass".to_string(),
            participant_ids: vec![1, 2, 3],
        };
        let normalized = tournament.normalize();

        assert_eq!(normalized.t_id, TournamentId(100));
        assert_eq!(normalized.name, "Don't Park on the Grass");
        assert!(normalized.brackets.is_empty());
        assert!(normalized.admins.is_empty());
    }
}

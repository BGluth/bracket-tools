use bracket_tools_startgg_schema::{
    get_games_for_set::{self, GetGamesOfSet},
    get_player_for_player_id::GetPlayerForPlayerId,
    get_tournament_for_id::GetTournamentForId,
};
use thiserror::Error;

use crate::gg_data_types::{
    GgCharacterSelection, HydratedGgGame, HydratedGgPlayer, HydratedGgSet,
    HydratedGgTournament, Matchup, SlotData, StartGgId,
};

#[derive(Debug, Error)]
pub enum GgConversionError {
    #[error("missing field `{field}` on `{entity}`")]
    MissingField {
        entity: &'static str,
        field: &'static str,
    },
    #[error("invalid ID: {0}")]
    InvalidId(String),
}

fn parse_gg_id(id: &cynic::Id) -> Result<StartGgId, GgConversionError> {
    id.inner()
        .parse::<u64>()
        .map_err(|_| GgConversionError::InvalidId(id.inner().to_string()))
}

trait Required<T> {
    fn required(self, entity: &'static str, field: &'static str) -> Result<T, GgConversionError>;
}

impl<T> Required<T> for Option<T> {
    fn required(self, entity: &'static str, field: &'static str) -> Result<T, GgConversionError> {
        self.ok_or(GgConversionError::MissingField { entity, field })
    }
}

/// Wraps a tournament query response with the known tournament ID (from query variables).
pub struct TournamentQueryResult {
    pub id: StartGgId,
    pub response: GetTournamentForId,
}

/// Wraps a player query response with the known player ID (from query variables).
pub struct PlayerQueryResult {
    pub id: StartGgId,
    pub response: GetPlayerForPlayerId,
}

/// Wraps a set/games query response with the known set ID (from query variables).
pub struct SetQueryResult {
    pub id: StartGgId,
    pub response: GetGamesOfSet,
}

impl TryFrom<TournamentQueryResult> for HydratedGgTournament {
    type Error = GgConversionError;

    fn try_from(result: TournamentQueryResult) -> Result<Self, Self::Error> {
        let tournament = result
            .response
            .tournament
            .required("GetTournamentForId", "tournament")?;

        let name = tournament.name.required("Tournament", "name")?;

        let participant_ids = tournament
            .participants
            .and_then(|pc| pc.nodes)
            .unwrap_or_default()
            .into_iter()
            .flatten()
            .filter_map(|p| p.player)
            .filter_map(|p| p.id)
            .filter_map(|id| parse_gg_id(&id).ok())
            .collect();

        Ok(HydratedGgTournament {
            id: result.id,
            name,
            participant_ids,
        })
    }
}

impl TryFrom<PlayerQueryResult> for HydratedGgPlayer {
    type Error = GgConversionError;

    fn try_from(result: PlayerQueryResult) -> Result<Self, Self::Error> {
        let player = result
            .response
            .player
            .required("GetPlayerForPlayerId", "player")?;

        let gamer_tag = player.gamer_tag.required("Player", "gamer_tag")?;

        Ok(HydratedGgPlayer {
            id: result.id,
            gamer_tag,
            prefix: player.prefix,
        })
    }
}

impl TryFrom<SetQueryResult> for HydratedGgSet {
    type Error = GgConversionError;

    fn try_from(result: SetQueryResult) -> Result<Self, Self::Error> {
        let set = result.response.set.required("GetGamesOfSet", "set")?;

        let games = set
            .games
            .unwrap_or_default()
            .into_iter()
            .flatten()
            .map(convert_game)
            .collect();

        let matchup = extract_matchup(set.slots);

        Ok(HydratedGgSet {
            id: result.id,
            completed_at: None,
            round: None,
            matchup,
            games,
        })
    }
}

fn convert_game(game: get_games_for_set::Game) -> HydratedGgGame {
    let selections = game
        .selections
        .unwrap_or_default()
        .into_iter()
        .flatten()
        .map(|sel| GgCharacterSelection {
            character_id: sel
                .character
                .and_then(|c| c.id)
                .and_then(|id| parse_gg_id(&id).ok()),
        })
        .collect();

    HydratedGgGame {
        id: None,
        winner_id: game.winner_id.map(|id| id as u64),
        selections,
    }
}

/// Tries to build a `Matchup::Singles` from two slots. Returns `None` if
/// fewer than two slots have valid entrant + player data.
fn extract_matchup(
    slots: Option<Vec<Option<get_games_for_set::SetSlot>>>,
) -> Option<Matchup> {
    let slots = slots.unwrap_or_default();
    let mut slot_iter = slots.iter().flatten();

    let left = extract_slot(slot_iter.next()?)?;
    let right = extract_slot(slot_iter.next()?)?;

    Some(Matchup::Singles { left, right })
}

fn extract_slot(slot: &get_games_for_set::SetSlot) -> Option<SlotData> {
    let standing = slot.standing.as_ref()?;
    let entrant = standing.entrant.as_ref()?;

    let entrant_id = parse_gg_id(entrant.id.as_ref()?).ok()?;
    let player_id = slot_player_id(entrant)?;
    let score = slot_score(standing);

    Some(SlotData {
        entrant_id,
        player_id,
        score,
    })
}

fn slot_player_id(entrant: &get_games_for_set::Entrant) -> Option<StartGgId> {
    let player = entrant
        .participants
        .as_ref()?
        .iter()
        .flatten()
        .next()?
        .player
        .as_ref()?;
    parse_gg_id(player.id.as_ref()?).ok()
}

fn slot_score(standing: &get_games_for_set::Standing) -> Option<f64> {
    standing.stats.as_ref()?.score.as_ref()?.value
}

#[cfg(test)]
mod tests {
    use super::{
        GgConversionError, HydratedGgPlayer, HydratedGgSet, HydratedGgTournament, Matchup,
        PlayerQueryResult, SetQueryResult, TournamentQueryResult,
    };
    use bracket_tools_startgg_schema::{
        get_games_for_set as gfs, get_player_for_player_id as gp, get_tournament_for_id as gt,
    };

    #[test]
    fn tournament_conversion() {
        let response = gt::GetTournamentForId {
            tournament: Some(gt::Tournament {
                name: Some("Genesis 9".to_string()),
                participants: Some(gt::ParticipantConnection {
                    nodes: Some(vec![
                        Some(gt::Participant {
                            player: Some(gt::Player {
                                id: Some(cynic::Id::new("42")),
                            }),
                        }),
                        Some(gt::Participant {
                            player: Some(gt::Player {
                                id: Some(cynic::Id::new("43")),
                            }),
                        }),
                    ]),
                }),
            }),
        };

        let result =
            HydratedGgTournament::try_from(TournamentQueryResult { id: 100, response }).unwrap();

        assert_eq!(result.id, 100);
        assert_eq!(result.name, "Genesis 9");
        assert_eq!(result.participant_ids, vec![42, 43]);
    }

    #[test]
    fn tournament_conversion_missing_tournament() {
        let response = gt::GetTournamentForId { tournament: None };
        let err =
            HydratedGgTournament::try_from(TournamentQueryResult { id: 100, response }).unwrap_err();

        assert!(matches!(
            err,
            GgConversionError::MissingField {
                entity: "GetTournamentForId",
                ..
            }
        ));
    }

    #[test]
    fn tournament_conversion_skips_invalid_ids() {
        let response = gt::GetTournamentForId {
            tournament: Some(gt::Tournament {
                name: Some("Test".to_string()),
                participants: Some(gt::ParticipantConnection {
                    nodes: Some(vec![
                        Some(gt::Participant {
                            player: Some(gt::Player {
                                id: Some(cynic::Id::new("not-a-number")),
                            }),
                        }),
                        Some(gt::Participant {
                            player: Some(gt::Player {
                                id: Some(cynic::Id::new("42")),
                            }),
                        }),
                    ]),
                }),
            }),
        };

        let result =
            HydratedGgTournament::try_from(TournamentQueryResult { id: 1, response }).unwrap();

        assert_eq!(result.participant_ids, vec![42]);
    }

    #[test]
    fn player_conversion() {
        let response = gp::GetPlayerForPlayerId {
            player: Some(gp::Player {
                prefix: Some("TSM".to_string()),
                gamer_tag: Some("Tweek".to_string()),
            }),
        };

        let result =
            HydratedGgPlayer::try_from(PlayerQueryResult { id: 42, response }).unwrap();

        assert_eq!(result.id, 42);
        assert_eq!(result.gamer_tag, "Tweek");
        assert_eq!(result.prefix, Some("TSM".to_string()));
    }

    #[test]
    fn player_conversion_no_prefix() {
        let response = gp::GetPlayerForPlayerId {
            player: Some(gp::Player {
                prefix: None,
                gamer_tag: Some("MkLeo".to_string()),
            }),
        };

        let result =
            HydratedGgPlayer::try_from(PlayerQueryResult { id: 1, response }).unwrap();

        assert_eq!(result.gamer_tag, "MkLeo");
        assert!(result.prefix.is_none());
    }

    #[test]
    fn player_conversion_missing_player() {
        let response = gp::GetPlayerForPlayerId { player: None };
        let err = HydratedGgPlayer::try_from(PlayerQueryResult { id: 1, response }).unwrap_err();

        assert!(matches!(err, GgConversionError::MissingField { .. }));
    }

    #[test]
    fn set_conversion() {
        let response = gfs::GetGamesOfSet {
            set: Some(gfs::Set {
                games: Some(vec![Some(gfs::Game {
                    winner_id: Some(100),
                    selections: Some(vec![Some(gfs::GameSelection {
                        character: Some(gfs::Character {
                            id: Some(cynic::Id::new("500")),
                        }),
                    })]),
                })]),
                slots: Some(vec![
                    Some(make_slot("100", "10", 3.0)),
                    Some(make_slot("200", "20", 1.0)),
                ]),
            }),
        };

        let result = HydratedGgSet::try_from(SetQueryResult { id: 50, response }).unwrap();

        assert_eq!(result.id, 50);
        let Matchup::Singles { ref left, ref right } = result.matchup.expect("should have matchup");
        assert_eq!(left.entrant_id, 100);
        assert_eq!(left.player_id, 10);
        assert_eq!(left.score, Some(3.0));
        assert_eq!(right.entrant_id, 200);
        assert_eq!(right.player_id, 20);
        assert_eq!(right.score, Some(1.0));
        assert_eq!(result.games.len(), 1);
        assert_eq!(result.games[0].winner_id, Some(100));
        assert_eq!(result.games[0].selections[0].character_id, Some(500));
        assert!(result.completed_at.is_none());
        assert!(result.round.is_none());
    }

    #[test]
    fn set_conversion_missing_set() {
        let response = gfs::GetGamesOfSet { set: None };
        let err = HydratedGgSet::try_from(SetQueryResult { id: 50, response }).unwrap_err();

        assert!(matches!(err, GgConversionError::MissingField { .. }));
    }

    #[test]
    fn set_conversion_empty_games_and_slots() {
        let response = gfs::GetGamesOfSet {
            set: Some(gfs::Set {
                games: None,
                slots: None,
            }),
        };

        let result = HydratedGgSet::try_from(SetQueryResult { id: 1, response }).unwrap();

        assert!(result.games.is_empty());
        assert!(result.matchup.is_none());
    }

    fn make_slot(entrant_id: &str, player_id: &str, score: f64) -> gfs::SetSlot {
        gfs::SetSlot {
            standing: Some(gfs::Standing {
                entrant: Some(gfs::Entrant {
                    id: Some(cynic::Id::new(entrant_id)),
                    participants: Some(vec![Some(gfs::Participant {
                        player: Some(gfs::Player {
                            id: Some(cynic::Id::new(player_id)),
                        }),
                    })]),
                }),
                stats: Some(gfs::StandingStats {
                    score: Some(gfs::Score {
                        value: Some(score),
                    }),
                }),
            }),
        }
    }
}

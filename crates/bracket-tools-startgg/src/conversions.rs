use bracket_tools_startgg_schema::{
    admin_probe::{self, AdminProbe},
    get_event_structure::{self, GetEventStructure},
    get_games_for_set::{self, GetGamesOfSet},
    get_player_for_player_id::GetPlayerForPlayerId,
    get_sets_for_event::{self, GetSetsForEvent},
    get_tournament_for_id::{self, GetTournamentForId},
    mark_set_called::MarkSetCalled,
    mark_set_in_progress::MarkSetInProgress,
    scalars::Timestamp,
};
use thiserror::Error;

use crate::gg_data_types::{GgCharacterSelection, HydratedGgGame, HydratedGgPlayer, HydratedGgSet, Matchup, SlotData, StartGgId};

#[derive(Debug, Error)]
pub enum GgConversionError {
    #[error("missing field `{field}` on `{entity}`")]
    MissingField { entity: &'static str, field: &'static str },
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

/// One page of a paginated connection: the items it yielded plus the total page
/// count reported by the connection's `pageInfo` (used to drive the fetch loop).
#[derive(Debug)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub total_pages: i32,
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

fn tournament_ref(response: &GetTournamentForId) -> Result<&get_tournament_for_id::Tournament, GgConversionError> {
    response.tournament.as_ref().required("GetTournamentForId", "tournament")
}

/// Pulls the participant player IDs out of a single page of a tournament's
/// `participants` connection, skipping any nodes with missing or unparseable IDs.
fn participant_ids_from_connection(participants: Option<&get_tournament_for_id::ParticipantConnection>) -> Vec<StartGgId> {
    participants
        .and_then(|pc| pc.nodes.as_ref())
        .map(|nodes| {
            nodes
                .iter()
                .flatten()
                .filter_map(|p| p.player.as_ref())
                .filter_map(|p| p.id.as_ref())
                .filter_map(|id| parse_gg_id(id).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Extracts one page of participant IDs plus the connection's total page count
/// (defaulting to 1 when `pageInfo` is absent). Suitable as the `extract_page`
/// argument to [`GGProvider::fetch_all_pages`](crate::provider).
pub fn extract_tournament_participants_page(response: &GetTournamentForId) -> Result<Page<StartGgId>, GgConversionError> {
    let participants = tournament_ref(response)?.participants.as_ref();
    let total_pages = participants
        .and_then(|pc| pc.page_info.as_ref())
        .and_then(|pi| pi.total_pages)
        .unwrap_or(1);

    Ok(Page {
        items: participant_ids_from_connection(participants),
        total_pages,
    })
}

/// Reads the tournament name, present on every page's response.
pub fn tournament_name(response: &GetTournamentForId) -> Result<String, GgConversionError> {
    tournament_ref(response)?.name.clone().required("Tournament", "name")
}

/// Extracts one page of an event's sets plus the connection's total page count
/// (defaulting to 1 when `pageInfo` is absent). Suitable as the `extract_page`
/// argument to [`GGProvider::fetch_all_pages`](crate::provider).
pub fn extract_event_sets_page(response: &GetSetsForEvent) -> Result<Page<get_sets_for_event::Set>, GgConversionError> {
    let sets = response.event.as_ref().required("GetSetsForEvent", "event")?.sets.as_ref();

    let total_pages = sets.and_then(|sc| sc.page_info.as_ref()).and_then(|pi| pi.total_pages).unwrap_or(1);
    let items = sets
        .and_then(|sc| sc.nodes.as_ref())
        .map(|nodes| nodes.iter().flatten().cloned().collect())
        .unwrap_or_default();

    Ok(Page { items, total_pages })
}

/// Unwraps the event from a structure query response.
pub fn extract_event_structure(response: GetEventStructure) -> Result<get_event_structure::Event, GgConversionError> {
    response.event.required("GetEventStructure", "event")
}

/// The set fields returned by the `markSet*` mutations.
///
/// `state` is start.gg's undocumented Int; values observed here are evidence
/// for the scheduler's state map. A non-numeric set ID (e.g. `preview_*`)
/// yields `id: None` rather than an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetMutationResult {
    pub id: Option<StartGgId>,
    pub state: Option<i32>,
    pub started_at: Option<Timestamp>,
    pub completed_at: Option<Timestamp>,
}

/// The two mutation payloads are module-local twins; one conversion serves both.
macro_rules! impl_set_mutation_result_from {
    ($set_ty:ty) => {
        impl From<$set_ty> for SetMutationResult {
            fn from(set: $set_ty) -> Self {
                Self {
                    id: set.id.as_ref().and_then(|id| id.inner().parse::<u64>().ok()),
                    state: set.state,
                    started_at: set.started_at,
                    completed_at: set.completed_at,
                }
            }
        }
    };
}

impl_set_mutation_result_from!(bracket_tools_startgg_schema::mark_set_called::Set);
impl_set_mutation_result_from!(bracket_tools_startgg_schema::mark_set_in_progress::Set);

/// Unwraps a `markSetCalled` mutation response.
pub fn extract_mark_set_called(response: MarkSetCalled) -> Result<SetMutationResult, GgConversionError> {
    response.mark_set_called.required("MarkSetCalled", "markSetCalled").map(Into::into)
}

/// Unwraps a `markSetInProgress` mutation response.
pub fn extract_mark_set_in_progress(response: MarkSetInProgress) -> Result<SetMutationResult, GgConversionError> {
    response
        .mark_set_in_progress
        .required("MarkSetInProgress", "markSetInProgress")
        .map(Into::into)
}

/// What the admin probe learned about the current token.
///
/// `admins` is start.gg's admin-only field: `None` means the server hid it
/// from this token — itself evidence of a non-admin. Non-numeric ids are
/// skipped rather than erroring (the probe is advisory).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminProbeResult {
    pub current_user: Option<StartGgId>,
    pub admins: Option<Vec<StartGgId>>,
}

impl AdminProbeResult {
    /// True only when both halves resolved and the token's user is listed.
    pub fn is_admin(&self) -> bool {
        match (&self.current_user, &self.admins) {
            (Some(me), Some(admins)) => admins.contains(me),
            _ => false,
        }
    }
}

/// Flattens an admin-probe response; infallible by design (absence is signal).
pub fn extract_admin_probe(response: AdminProbe) -> AdminProbeResult {
    let parse = |id: admin_probe::User| id.id.and_then(|id| id.inner().parse::<u64>().ok());
    AdminProbeResult {
        current_user: response.current_user.and_then(parse),
        admins: response
            .tournament
            .and_then(|t| t.admins)
            .map(|admins| admins.into_iter().flatten().filter_map(parse).collect()),
    }
}

impl TryFrom<PlayerQueryResult> for HydratedGgPlayer {
    type Error = GgConversionError;

    fn try_from(result: PlayerQueryResult) -> Result<Self, Self::Error> {
        let player = result.response.player.required("GetPlayerForPlayerId", "player")?;

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

        let games = set.games.unwrap_or_default().into_iter().flatten().map(convert_game).collect();

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
            character_id: sel.character.and_then(|c| c.id).and_then(|id| parse_gg_id(&id).ok()),
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
fn extract_matchup(slots: Option<Vec<Option<get_games_for_set::SetSlot>>>) -> Option<Matchup> {
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
    let player = entrant.participants.as_ref()?.iter().flatten().next()?.player.as_ref()?;
    parse_gg_id(player.id.as_ref()?).ok()
}

fn slot_score(standing: &get_games_for_set::Standing) -> Option<f64> {
    standing.stats.as_ref()?.score.as_ref()?.value
}

#[cfg(test)]
mod tests {
    use bracket_tools_startgg_schema::{
        enums::ActivityState,
        get_event_structure as ges, get_games_for_set as gfs, get_player_for_player_id as gp, get_sets_for_event as gse,
        get_tournament_for_id as gt, mark_set_called as msc, mark_set_in_progress as msip,
        scalars::{Id, Timestamp},
    };

    use super::{
        extract_event_sets_page, extract_event_structure, extract_mark_set_called, extract_mark_set_in_progress,
        extract_tournament_participants_page, tournament_name, GgConversionError, HydratedGgPlayer, HydratedGgSet, Matchup,
        PlayerQueryResult, SetMutationResult, SetQueryResult,
    };

    fn participant(id: &str) -> Option<gt::Participant> {
        Some(gt::Participant {
            player: Some(gt::Player {
                id: Some(cynic::Id::new(id)),
            }),
        })
    }

    #[test]
    fn tournament_page_extraction() {
        let response = gt::GetTournamentForId {
            tournament: Some(gt::Tournament {
                name: Some("Genesis 9".to_string()),
                participants: Some(gt::ParticipantConnection {
                    page_info: Some(gt::PageInfo { total_pages: Some(3) }),
                    nodes: Some(vec![participant("42"), participant("43")]),
                }),
            }),
        };

        assert_eq!(tournament_name(&response).unwrap(), "Genesis 9");

        let page = extract_tournament_participants_page(&response).unwrap();
        assert_eq!(page.items, vec![42, 43]);
        assert_eq!(page.total_pages, 3);
    }

    #[test]
    fn tournament_total_pages_defaults_to_one_without_page_info() {
        let response = gt::GetTournamentForId {
            tournament: Some(gt::Tournament {
                name: Some("Test".to_string()),
                participants: Some(gt::ParticipantConnection {
                    page_info: None,
                    nodes: Some(vec![]),
                }),
            }),
        };

        let page = extract_tournament_participants_page(&response).unwrap();
        assert!(page.items.is_empty());
        assert_eq!(page.total_pages, 1);
    }

    #[test]
    fn tournament_page_skips_invalid_ids() {
        let response = gt::GetTournamentForId {
            tournament: Some(gt::Tournament {
                name: Some("Test".to_string()),
                participants: Some(gt::ParticipantConnection {
                    page_info: None,
                    nodes: Some(vec![participant("not-a-number"), participant("42")]),
                }),
            }),
        };

        assert_eq!(extract_tournament_participants_page(&response).unwrap().items, vec![42]);
    }

    #[test]
    fn tournament_extraction_missing_tournament() {
        let response = gt::GetTournamentForId { tournament: None };

        for err in [
            extract_tournament_participants_page(&response).unwrap_err(),
            tournament_name(&response).unwrap_err(),
        ] {
            assert!(matches!(
                err,
                GgConversionError::MissingField {
                    entity: "GetTournamentForId",
                    ..
                }
            ));
        }
    }

    #[test]
    fn tournament_name_missing() {
        let response = gt::GetTournamentForId {
            tournament: Some(gt::Tournament {
                name: None,
                participants: None,
            }),
        };

        assert!(matches!(
            tournament_name(&response).unwrap_err(),
            GgConversionError::MissingField {
                entity: "Tournament",
                field: "name",
            }
        ));
    }

    #[test]
    fn player_conversion() {
        let response = gp::GetPlayerForPlayerId {
            player: Some(gp::Player {
                prefix: Some("TSM".to_string()),
                gamer_tag: Some("Tweek".to_string()),
            }),
        };

        let result = HydratedGgPlayer::try_from(PlayerQueryResult { id: 42, response }).unwrap();

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

        let result = HydratedGgPlayer::try_from(PlayerQueryResult { id: 1, response }).unwrap();

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
                slots: Some(vec![Some(make_slot("100", "10", 3.0)), Some(make_slot("200", "20", 1.0))]),
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
            set: Some(gfs::Set { games: None, slots: None }),
        };

        let result = HydratedGgSet::try_from(SetQueryResult { id: 1, response }).unwrap();

        assert!(result.games.is_empty());
        assert!(result.matchup.is_none());
    }

    fn event_set(id: &str) -> Option<gse::Set> {
        Some(gse::Set {
            id: Some(Id::new(id)),
            state: Some(1),
            round: Some(1),
            identifier: Some("A".to_string()),
            full_round_text: Some("Winners Round 1".to_string()),
            started_at: None,
            completed_at: None,
            winner_id: None,
            has_placeholder: Some(false),
            phase_group: None,
            slots: None,
        })
    }

    fn sets_response(page_info: Option<gse::PageInfo>, nodes: Option<Vec<Option<gse::Set>>>) -> gse::GetSetsForEvent {
        gse::GetSetsForEvent {
            event: Some(gse::Event {
                sets: Some(gse::SetConnection { page_info, nodes }),
            }),
        }
    }

    #[test]
    fn event_sets_page_extraction() {
        let response = sets_response(
            Some(gse::PageInfo { total_pages: Some(4) }),
            Some(vec![event_set("1"), event_set("2")]),
        );

        let page = extract_event_sets_page(&response).unwrap();
        assert_eq!(page.items.len(), 2);
        assert_eq!(page.total_pages, 4);
    }

    #[test]
    fn event_sets_page_defaults_to_one_page_without_page_info() {
        let page = extract_event_sets_page(&sets_response(None, None)).unwrap();
        assert!(page.items.is_empty());
        assert_eq!(page.total_pages, 1);
    }

    #[test]
    fn event_sets_page_missing_event() {
        let response = gse::GetSetsForEvent { event: None };

        assert!(matches!(
            extract_event_sets_page(&response).unwrap_err(),
            GgConversionError::MissingField {
                entity: "GetSetsForEvent",
                ..
            }
        ));
    }

    #[test]
    fn event_structure_extraction() {
        let response = ges::GetEventStructure {
            event: Some(ges::Event {
                id: Some(Id::new("100")),
                name: Some("Ultimate Singles".to_string()),
                state: Some(ActivityState::Active),
                start_at: Some(Timestamp(1751234567)),
                tournament: None,
                phases: None,
                phase_groups: None,
                num_entrants: Some(32),
            }),
        };

        let event = extract_event_structure(response).unwrap();
        assert_eq!(event.name.as_deref(), Some("Ultimate Singles"));
        assert_eq!(event.num_entrants, Some(32));
    }

    #[test]
    fn event_structure_missing_event() {
        let response = ges::GetEventStructure { event: None };

        assert!(matches!(
            extract_event_structure(response).unwrap_err(),
            GgConversionError::MissingField {
                entity: "GetEventStructure",
                ..
            }
        ));
    }

    #[test]
    fn mark_set_called_extraction() {
        let response = msc::MarkSetCalled {
            mark_set_called: Some(msc::Set {
                id: Some(Id::new("777")),
                state: Some(6),
                started_at: Some(Timestamp(1751234567)),
                completed_at: None,
            }),
        };

        let result = extract_mark_set_called(response).unwrap();
        assert_eq!(
            result,
            SetMutationResult {
                id: Some(777),
                state: Some(6),
                started_at: Some(Timestamp(1751234567)),
                completed_at: None,
            }
        );
    }

    #[test]
    fn mark_set_in_progress_extraction() {
        let response = msip::MarkSetInProgress {
            mark_set_in_progress: Some(msip::Set {
                id: Some(Id::new("888")),
                state: Some(2),
                started_at: None,
                completed_at: None,
            }),
        };

        let result = extract_mark_set_in_progress(response).unwrap();
        assert_eq!(result.id, Some(888));
        assert_eq!(result.state, Some(2));
    }

    #[test]
    fn mark_set_called_missing_payload() {
        let response = msc::MarkSetCalled { mark_set_called: None };

        assert!(matches!(
            extract_mark_set_called(response).unwrap_err(),
            GgConversionError::MissingField {
                entity: "MarkSetCalled",
                ..
            }
        ));
    }

    #[test]
    fn set_mutation_result_keeps_non_numeric_id_as_none() {
        let set = msc::Set {
            id: Some(Id::new("preview_123_45")),
            state: None,
            started_at: None,
            completed_at: None,
        };

        assert_eq!(SetMutationResult::from(set).id, None);
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
                    score: Some(gfs::Score { value: Some(score) }),
                }),
            }),
        }
    }
}

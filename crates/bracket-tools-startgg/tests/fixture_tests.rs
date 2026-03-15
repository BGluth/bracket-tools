use bracket_tools_core::data_types::Normalizable;
use bracket_tools_core::types::{GameType, GameWinningSide, PlayerId};
use bracket_tools_startgg::conversions::{PlayerQueryResult, SetQueryResult, TournamentQueryResult};
use bracket_tools_startgg::gg_data_types::{
    HydratedGgPlayer, HydratedGgSet, HydratedGgTournament, Matchup,
};
use bracket_tools_startgg_schema::{
    get_games_for_set::GetGamesOfSet, get_player_for_player_id::GetPlayerForPlayerId,
    get_tournament_for_id::GetTournamentForId,
};

fn load_set_fixture(json: &str, set_id: u64) -> HydratedGgSet {
    let response: cynic::GraphQlResponse<GetGamesOfSet> =
        serde_json::from_str(json).expect("fixture should deserialize");
    let data = response.data.expect("fixture should have data");
    HydratedGgSet::try_from(SetQueryResult {
        id: set_id,
        response: data,
    })
    .expect("conversion should succeed")
}

fn load_tournament_fixture(json: &str, tournament_id: u64) -> HydratedGgTournament {
    let response: cynic::GraphQlResponse<GetTournamentForId> =
        serde_json::from_str(json).expect("fixture should deserialize");
    let data = response.data.expect("fixture should have data");
    HydratedGgTournament::try_from(TournamentQueryResult {
        id: tournament_id,
        response: data,
    })
    .expect("conversion should succeed")
}

fn load_player_fixture(json: &str, player_id: u64) -> HydratedGgPlayer {
    let response: cynic::GraphQlResponse<GetPlayerForPlayerId> =
        serde_json::from_str(json).expect("fixture should deserialize");
    let data = response.data.expect("fixture should have data");
    HydratedGgPlayer::try_from(PlayerQueryResult {
        id: player_id,
        response: data,
    })
    .expect("conversion should succeed")
}

/// Asserts that every game's `winner_id` matches one of the entrant IDs in the matchup.
/// This is the core validation for the Entrant.id fix: `winner_id` is an entrant ID,
/// not a player ID.
fn assert_winner_ids_are_entrant_ids(set: &HydratedGgSet) {
    let Matchup::Singles { ref left, ref right } = set.matchup.as_ref().expect("should have matchup");
    for game in &set.games {
        let winner = game.winner_id.expect("game should have winner");
        assert!(
            winner == left.entrant_id || winner == right.entrant_id,
            "winnerId {winner} should match an entrant ID ({} or {}), not a player ID ({} or {})",
            left.entrant_id,
            right.entrant_id,
            left.player_id,
            right.player_id,
        );
    }
}

/// Asserts that normalized games use player IDs (not entrant IDs) for PlayerId.
fn assert_normalized_uses_player_ids(set: &HydratedGgSet) {
    let Matchup::Singles { ref left, ref right } = set.matchup.as_ref().expect("should have matchup");
    let normalized = set.normalize();
    for game in &normalized.games {
        match &game.g_type {
            GameType::OneVOne(p1, p2) => {
                assert_eq!(p1.p_id, PlayerId(left.player_id));
                assert_eq!(p2.p_id, PlayerId(right.player_id));
            }
            _ => panic!("expected OneVOne"),
        }
    }
}

// Bread Basket 2025 — AceEagle 2-0 TC | dog
#[test]
fn set_aceeagle_v_dog() {
    let set = load_set_fixture(
        include_str!("fixtures/set_89833288_aceeagle_v_dog.json"),
        89833288,
    );

    assert_eq!(set.games.len(), 2);
    assert_winner_ids_are_entrant_ids(&set);
    assert_normalized_uses_player_ids(&set);

    let Matchup::Singles { ref left, ref right } = set.matchup.as_ref().unwrap();
    assert_ne!(left.entrant_id, left.player_id);
    assert_ne!(right.entrant_id, right.player_id);

    // AceEagle won both games (right slot, entrant 19524217)
    let normalized = set.normalize();
    assert_eq!(normalized.games.len(), 2);
    assert!(matches!(normalized.games[0].winning_side, GameWinningSide::Right));
    assert!(matches!(normalized.games[1].winning_side, GameWinningSide::Right));
}

// Bread Basket 2025 — Spitfire 2-0 Dest
#[test]
fn set_spitfire_v_dest() {
    let set = load_set_fixture(
        include_str!("fixtures/set_89840397_spitfire_v_dest.json"),
        89840397,
    );

    assert_eq!(set.games.len(), 2);
    assert_winner_ids_are_entrant_ids(&set);
    assert_normalized_uses_player_ids(&set);
}

// Bread Basket 2025 — Devintheguy 2-1 Sancocho
#[test]
fn set_devintheguy_v_sancocho() {
    let set = load_set_fixture(
        include_str!("fixtures/set_89845364_devintheguy_v_sancocho.json"),
        89845364,
    );

    assert_eq!(set.games.len(), 3);
    assert_winner_ids_are_entrant_ids(&set);
    assert_normalized_uses_player_ids(&set);

    let normalized = set.normalize();
    assert_eq!(normalized.games.len(), 3);
}

// Bread Basket 2025 — FeiFFer 2-0 YaBoiChips
#[test]
fn set_yaboichips_v_feiffer() {
    let set = load_set_fixture(
        include_str!("fixtures/set_89833281_yaboichips_v_feiffer.json"),
        89833281,
    );

    assert_eq!(set.games.len(), 2);
    assert_winner_ids_are_entrant_ids(&set);
    assert_normalized_uses_player_ids(&set);
}

// Bread Basket 2025 — Bandage 1-2 nick
#[test]
fn set_bandage_v_nick() {
    let set = load_set_fixture(
        include_str!("fixtures/set_89833550_bandage_v_nick.json"),
        89833550,
    );

    assert_eq!(set.games.len(), 3);
    assert_winner_ids_are_entrant_ids(&set);
    assert_normalized_uses_player_ids(&set);
}

// Bread Basket 2025 — ./rust_man 1-2 DaBeef
#[test]
fn set_rustman_v_dabeef() {
    let set = load_set_fixture(
        include_str!("fixtures/set_89833279_rustman_v_dabeef.json"),
        89833279,
    );

    assert_eq!(set.games.len(), 3);
    assert_winner_ids_are_entrant_ids(&set);
    assert_normalized_uses_player_ids(&set);
}

// Colchester Clash #17 — CraigleRock 1-2 Eternity
#[test]
fn set_craiglerock_v_eternity() {
    let set = load_set_fixture(
        include_str!("fixtures/set_95304560_craiglerock_v_eternity.json"),
        95304560,
    );

    assert_eq!(set.games.len(), 3);
    assert_winner_ids_are_entrant_ids(&set);
    assert_normalized_uses_player_ids(&set);

    let Matchup::Singles { ref left, ref right } = set.matchup.as_ref().unwrap();
    // CraigleRock is left (entrant 21625594, player 3551055)
    assert_eq!(left.entrant_id, 21625594);
    assert_eq!(left.player_id, 3551055);
    assert_eq!(left.score, Some(1.0));
    // Eternity is right (entrant 21623263, player 57840)
    assert_eq!(right.entrant_id, 21623263);
    assert_eq!(right.player_id, 57840);
    assert_eq!(right.score, Some(2.0));
}

// Colchester Clash #17 — Soufflé 2-0 Soul Badguy
#[test]
fn set_soulbadguy_v_souffle() {
    let set = load_set_fixture(
        include_str!("fixtures/set_95304563_soulbadguy_v_souffle.json"),
        95304563,
    );

    assert_eq!(set.games.len(), 2);
    assert_winner_ids_are_entrant_ids(&set);
    assert_normalized_uses_player_ids(&set);
}

// French Bread Rumble #86 — Utugboat 3-1 Cronos
#[test]
fn set_cronos_v_utugboat() {
    let set = load_set_fixture(
        include_str!("fixtures/set_100308057_cronos_v_utugboat.json"),
        100308057,
    );

    assert_eq!(set.games.len(), 4);
    assert_winner_ids_are_entrant_ids(&set);
    assert_normalized_uses_player_ids(&set);

    // Verify character selections are present
    for game in &set.games {
        assert_eq!(game.selections.len(), 2);
        for sel in &game.selections {
            assert!(sel.character_id.is_some());
        }
    }

    let normalized = set.normalize();
    assert_eq!(normalized.games.len(), 4);
}

// Reals Arena #23 — Perendeshe 3-2 Spectra (Grand Finals)
#[test]
fn set_perendeshe_v_spectra_gf() {
    let set = load_set_fixture(
        include_str!("fixtures/set_98613766_perendeshe_spectra_gf.json"),
        98613766,
    );

    assert_eq!(set.games.len(), 5);
    assert_winner_ids_are_entrant_ids(&set);
    assert_normalized_uses_player_ids(&set);

    let Matchup::Singles { ref left, ref right } = set.matchup.as_ref().unwrap();
    // Perendeshe: entrant 22438696, player 2325613, score 3
    assert_eq!(left.entrant_id, 22438696);
    assert_eq!(left.player_id, 2325613);
    assert_eq!(left.score, Some(3.0));
    // Spectra: entrant 22435413, player 4548139, score 2
    assert_eq!(right.entrant_id, 22435413);
    assert_eq!(right.player_id, 4548139);
    assert_eq!(right.score, Some(2.0));

    let normalized = set.normalize();
    assert_eq!(normalized.games.len(), 5);
}

// Bread Basket 2025 tournament
#[test]
fn tournament_bread_basket_2025() {
    let tournament = load_tournament_fixture(
        include_str!("fixtures/tournament_733586_bread_basket_2025.json"),
        733586,
    );

    assert_eq!(tournament.id, 733586);
    assert_eq!(tournament.name, "Bread Basket - 2025");
    assert!(!tournament.participant_ids.is_empty());
}

// Player: Cronos
#[test]
fn player_cronos() {
    let player = load_player_fixture(
        include_str!("fixtures/player_4665060_cronos.json"),
        4665060,
    );

    assert_eq!(player.id, 4665060);
    assert_eq!(player.gamer_tag, "Cronos");

    let normalized = player.normalize();
    assert_eq!(normalized.name, "Cronos");
}

// Player: CraigleRok
#[test]
fn player_craiglerok() {
    let player = load_player_fixture(
        include_str!("fixtures/player_3551055_craiglerock.json"),
        3551055,
    );

    assert_eq!(player.id, 3551055);
    assert_eq!(player.gamer_tag, "CraigleRok");

    let normalized = player.normalize();
    assert_eq!(normalized.name, "CraigleRok");
}
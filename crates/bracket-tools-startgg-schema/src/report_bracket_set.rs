//! The set-reporting mutation: winner, optional per-game data (winners and
//! character selections), and the DQ flag. Returns the affected sets (the
//! reported set plus any sets the result advanced).

use crate::{
    scalars::{Id, Timestamp},
    schema::schema,
};

#[derive(cynic::QueryVariables, Debug)]
pub struct ReportBracketSetVariables<'a> {
    pub set_id: &'a cynic::Id,
    pub winner_id: Option<cynic::Id>,
    pub is_dq: Option<bool>,
    pub game_data: Option<Vec<BracketSetGameDataInput>>,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Mutation", variables = "ReportBracketSetVariables")]
pub struct ReportBracketSet {
    #[arguments(setId: $set_id, winnerId: $winner_id, isDQ: $is_dq, gameData: $game_data)]
    pub report_bracket_set: Option<Vec<Option<Set>>>,
}

/// Same 4-field payload the mark-set mutations use.
#[derive(cynic::QueryFragment, Debug)]
pub struct Set {
    pub id: Option<Id>,
    pub state: Option<i32>,
    pub started_at: Option<Timestamp>,
    pub completed_at: Option<Timestamp>,
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct BracketSetGameDataInput {
    pub winner_id: Option<cynic::Id>,
    pub game_num: i32,
    pub entrant1_score: Option<i32>,
    pub entrant2_score: Option<i32>,
    pub stage_id: Option<cynic::Id>,
    pub selections: Option<Vec<BracketSetGameSelectionInput>>,
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct BracketSetGameSelectionInput {
    pub entrant_id: cynic::Id,
    pub character_id: Option<i32>,
}

#[cfg(test)]
mod tests {
    use cynic::MutationBuilder;

    use super::{BracketSetGameDataInput, BracketSetGameSelectionInput, ReportBracketSet, ReportBracketSetVariables};

    #[test]
    fn builds_a_mutation_operation() {
        let set_id = cynic::Id::new("12345");
        let operation = ReportBracketSet::build(ReportBracketSetVariables {
            set_id: &set_id,
            winner_id: Some(cynic::Id::new("111")),
            is_dq: Some(false),
            game_data: Some(vec![BracketSetGameDataInput {
                winner_id: Some(cynic::Id::new("111")),
                game_num: 1,
                entrant1_score: None,
                entrant2_score: None,
                stage_id: None,
                selections: Some(vec![BracketSetGameSelectionInput {
                    entrant_id: cynic::Id::new("111"),
                    character_id: Some(7),
                }]),
            }]),
        });

        assert!(operation.query.starts_with("mutation"));
        assert!(operation.query.contains("reportBracketSet"));
        assert!(operation.query.contains("isDQ"));
        assert!(operation.query.contains("gameData"));
    }
}

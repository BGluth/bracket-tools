//! The admin half of the add-attendee flow: mint a registration token on
//! behalf of a user for a set of events (requires tournament-admin rights).
//! The token is redeemed by `registerForTournament`. The tournament itself is
//! implied by the event ids.

use crate::schema::schema;

#[derive(cynic::QueryVariables, Debug)]
pub struct GenerateRegistrationTokenVariables<'a> {
    pub user_id: &'a cynic::Id,
    pub registration: TournamentRegistrationInput,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Mutation", variables = "GenerateRegistrationTokenVariables")]
pub struct GenerateRegistrationToken {
    #[arguments(userId: $user_id, registration: $registration)]
    pub generate_registration_token: Option<String>,
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct TournamentRegistrationInput {
    pub event_ids: Option<Vec<Option<cynic::Id>>>,
}

#[cfg(test)]
mod tests {
    use cynic::MutationBuilder;

    use super::{GenerateRegistrationToken, GenerateRegistrationTokenVariables, TournamentRegistrationInput};

    #[test]
    fn builds_a_mutation_operation() {
        let user_id = cynic::Id::new("123456");
        let operation = GenerateRegistrationToken::build(GenerateRegistrationTokenVariables {
            user_id: &user_id,
            registration: TournamentRegistrationInput {
                event_ids: Some(vec![Some(cynic::Id::new("111"))]),
            },
        });

        assert!(operation.query.starts_with("mutation"));
        assert!(operation.query.contains("generateRegistrationToken"));

        let variables = serde_json::to_string(&operation.variables).unwrap();
        assert!(variables.contains("eventIds"));
    }
}

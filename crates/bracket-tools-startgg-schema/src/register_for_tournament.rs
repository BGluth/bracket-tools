//! Redeems a registration token minted by `generateRegistrationToken`,
//! completing the on-behalf-of registration for the events the token was
//! minted with. Returns the resulting participant.

use crate::{generate_registration_token::TournamentRegistrationInput, scalars::Id, schema::schema};

#[derive(cynic::QueryVariables, Debug)]
pub struct RegisterForTournamentVariables<'a> {
    pub registration: TournamentRegistrationInput,
    pub registration_token: &'a str,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Mutation", variables = "RegisterForTournamentVariables")]
pub struct RegisterForTournament {
    #[arguments(registration: $registration, registrationToken: $registration_token)]
    pub register_for_tournament: Option<Participant>,
}

#[derive(cynic::QueryFragment, Debug)]
pub struct Participant {
    pub id: Option<Id>,
    pub gamer_tag: Option<String>,
}

#[cfg(test)]
mod tests {
    use cynic::MutationBuilder;

    use super::{RegisterForTournament, RegisterForTournamentVariables, TournamentRegistrationInput};

    #[test]
    fn builds_a_mutation_operation() {
        let operation = RegisterForTournament::build(RegisterForTournamentVariables {
            registration: TournamentRegistrationInput {
                event_ids: Some(vec![Some(cynic::Id::new("111"))]),
            },
            registration_token: "tok",
        });

        assert!(operation.query.starts_with("mutation"));
        assert!(operation.query.contains("registerForTournament"));
        assert!(operation.query.contains("registrationToken"));
    }
}

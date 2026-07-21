//! The admin roster query: a tournament's paginated participant list with the
//! fields the TO desk needs (tag, user id, per-participant events, check-in),
//! plus the tournament header (identity, start date, event list — the
//! slug → id vocabulary for registration mutations).
//!
//! The optional `unpaid` filter is server-side; `Participant` has no paid
//! field, so filtering is the only view of payment state the public API
//! exposes (and there is no mutation to change it).

use crate::{
    scalars::{Id, Timestamp},
    schema::schema,
};

#[derive(cynic::QueryVariables, Debug)]
pub struct GetParticipantsForTournamentVariables<'a> {
    pub slug: &'a str,
    pub page: i32,
    pub per_page: i32,
    pub unpaid: Option<bool>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "Query", variables = "GetParticipantsForTournamentVariables")]
pub struct GetParticipantsForTournament {
    #[arguments(slug: $slug)]
    pub tournament: Option<Tournament>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(variables = "GetParticipantsForTournamentVariables")]
pub struct Tournament {
    pub id: Option<Id>,
    pub name: Option<String>,
    pub start_at: Option<Timestamp>,
    pub events: Option<Vec<Option<Event>>>,
    #[arguments(query: { page: $page, perPage: $per_page, filter: { unpaid: $unpaid } })]
    pub participants: Option<ParticipantConnection>,
}

/// Serves both the tournament's event list and each participant's events.
#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct Event {
    pub id: Option<Id>,
    pub slug: Option<String>,
    pub name: Option<String>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct ParticipantConnection {
    pub page_info: Option<PageInfo>,
    pub nodes: Option<Vec<Option<Participant>>>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct PageInfo {
    pub total_pages: Option<i32>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct Participant {
    pub id: Option<Id>,
    pub gamer_tag: Option<String>,
    pub prefix: Option<String>,
    pub checked_in: Option<bool>,
    pub verified: Option<bool>,
    pub user: Option<User>,
    pub events: Option<Vec<Option<Event>>>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct User {
    pub id: Option<Id>,
    pub slug: Option<String>,
}

#[cfg(test)]
mod tests {
    use cynic::QueryBuilder;

    use super::{GetParticipantsForTournament, GetParticipantsForTournamentVariables};

    #[test]
    fn builds_a_query_operation() {
        let operation = GetParticipantsForTournament::build(GetParticipantsForTournamentVariables {
            slug: "tournament/french-bread-rumble-100",
            page: 1,
            per_page: 100,
            unpaid: Some(true),
        });

        assert!(operation.query.contains("participants"));
        assert!(operation.query.contains("unpaid"));
        assert!(operation.query.contains("gamerTag"));
        assert!(operation.query.contains("checkedIn"));
        assert!(operation.query.contains("startAt"));
    }
}

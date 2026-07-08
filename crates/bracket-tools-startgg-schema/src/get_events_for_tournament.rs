//! A tournament's event list — expands a tournament slug into the per-event
//! slugs every other query takes.

use crate::{scalars::Id, schema::schema};

#[derive(cynic::QueryVariables, Debug)]
pub struct GetEventsForTournamentVariables<'a> {
    pub slug: &'a str,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "Query", variables = "GetEventsForTournamentVariables")]
pub struct GetEventsForTournament {
    #[arguments(slug: $slug)]
    pub tournament: Option<Tournament>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct Tournament {
    pub id: Option<Id>,
    pub slug: Option<String>,
    pub events: Option<Vec<Option<Event>>>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct Event {
    pub id: Option<Id>,
    pub slug: Option<String>,
    pub name: Option<String>,
}

#[cfg(test)]
mod tests {
    use cynic::QueryBuilder;

    use super::{GetEventsForTournament, GetEventsForTournamentVariables};

    #[test]
    fn builds_a_query_operation() {
        let operation = GetEventsForTournament::build(GetEventsForTournamentVariables {
            slug: "tournament/french-bread-rumble-100",
        });

        assert!(operation.query.contains("tournament"));
        assert!(operation.query.contains("events"));
    }
}

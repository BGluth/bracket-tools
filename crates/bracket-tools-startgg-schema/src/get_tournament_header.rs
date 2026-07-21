//! Lightweight tournament header: identity, start date, and owner — the seed
//! for series discovery via the `tournaments` owner filter, without paying
//! for a roster fetch.

use crate::{
    scalars::{Id, Timestamp},
    schema::schema,
};

#[derive(cynic::QueryVariables, Debug)]
pub struct GetTournamentHeaderVariables<'a> {
    pub slug: &'a str,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "Query", variables = "GetTournamentHeaderVariables")]
pub struct GetTournamentHeader {
    #[arguments(slug: $slug)]
    pub tournament: Option<Tournament>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct Tournament {
    pub id: Option<Id>,
    pub name: Option<String>,
    pub slug: Option<String>,
    pub start_at: Option<Timestamp>,
    pub owner: Option<User>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct User {
    pub id: Option<Id>,
}

#[cfg(test)]
mod tests {
    use cynic::QueryBuilder;

    use super::{GetTournamentHeader, GetTournamentHeaderVariables};

    #[test]
    fn builds_a_query_operation() {
        let operation = GetTournamentHeader::build(GetTournamentHeaderVariables {
            slug: "tournament/french-bread-rumble-100",
        });

        assert!(operation.query.contains("tournament"));
        assert!(operation.query.contains("owner"));
        assert!(operation.query.contains("startAt"));
    }
}

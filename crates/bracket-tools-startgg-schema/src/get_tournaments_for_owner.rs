//! Paginated tournament listing, filtered by owner (series discovery) or by
//! start.gg's `isCurrentUserAdmin` flag (every tournament the token's user
//! helps run). Null filter halves are simply omitted server-side, so one
//! query serves both shapes.

use crate::{
    scalars::{Id, Timestamp},
    schema::schema,
};

#[derive(cynic::QueryVariables, Debug)]
pub struct GetTournamentsForOwnerVariables {
    pub owner_id: Option<cynic::Id>,
    pub admin_only: Option<bool>,
    pub page: i32,
    pub per_page: i32,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "Query", variables = "GetTournamentsForOwnerVariables")]
pub struct GetTournamentsForOwner {
    #[arguments(query: { page: $page, perPage: $per_page, filter: { ownerId: $owner_id, isCurrentUserAdmin: $admin_only } })]
    pub tournaments: Option<TournamentConnection>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct TournamentConnection {
    pub page_info: Option<PageInfo>,
    pub nodes: Option<Vec<Option<Tournament>>>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct PageInfo {
    pub total_pages: Option<i32>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct Tournament {
    pub id: Option<Id>,
    pub name: Option<String>,
    pub slug: Option<String>,
    pub start_at: Option<Timestamp>,
}

#[cfg(test)]
mod tests {
    use cynic::QueryBuilder;

    use super::{GetTournamentsForOwner, GetTournamentsForOwnerVariables};

    #[test]
    fn builds_a_query_operation() {
        let operation = GetTournamentsForOwner::build(GetTournamentsForOwnerVariables {
            owner_id: Some(cynic::Id::new("123")),
            admin_only: None,
            page: 1,
            per_page: 50,
        });

        assert!(operation.query.contains("tournaments"));
        assert!(operation.query.contains("ownerId"));
        assert!(operation.query.contains("isCurrentUserAdmin"));
        assert!(operation.query.contains("totalPages"));
    }
}

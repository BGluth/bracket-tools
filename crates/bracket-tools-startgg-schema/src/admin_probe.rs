//! The preflight admin probe: one query answering both "who is this token?"
//! (`currentUser`) and "who administers this tournament?" (`Tournament.admins`,
//! an admin-only field — hidden/null for non-admin tokens, which is itself
//! signal). Together they decide writes-armed vs advisor-only before doors.

use crate::{scalars::Id, schema::schema};

#[derive(cynic::QueryVariables, Debug)]
pub struct AdminProbeVariables<'a> {
    pub tournament_id: &'a cynic::Id,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "Query", variables = "AdminProbeVariables")]
pub struct AdminProbe {
    pub current_user: Option<User>,
    #[arguments(id: $tournament_id)]
    pub tournament: Option<Tournament>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct Tournament {
    pub id: Option<Id>,
    pub admins: Option<Vec<Option<User>>>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct User {
    pub id: Option<Id>,
}

//! A user's public profile by numeric id: slug (the profile URL) and current
//! tag. Turns a bare user id from `find` into something an operator can
//! recognise and look up in the start.gg admin UI.

use crate::{scalars::Id, schema::schema};

#[derive(cynic::QueryVariables, Debug)]
pub struct GetUserProfileVariables<'a> {
    pub user_id: &'a cynic::Id,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "Query", variables = "GetUserProfileVariables")]
pub struct GetUserProfile {
    #[arguments(id: $user_id)]
    pub user: Option<User>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct User {
    pub id: Option<Id>,
    pub slug: Option<String>,
    pub player: Option<Player>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct Player {
    pub gamer_tag: Option<String>,
    pub prefix: Option<String>,
}

#[cfg(test)]
mod tests {
    use cynic::QueryBuilder;

    use super::{GetUserProfile, GetUserProfileVariables};

    #[test]
    fn builds_a_query_operation() {
        let user_id = cynic::Id::new("1808768");
        let operation = GetUserProfile::build(GetUserProfileVariables { user_id: &user_id });

        assert!(operation.query.contains("user"));
        assert!(operation.query.contains("slug"));
        assert!(operation.query.contains("gamerTag"));
    }
}

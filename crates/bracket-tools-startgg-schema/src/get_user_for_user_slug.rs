use crate::schema::schema;

#[derive(cynic::QueryVariables, Debug)]
pub struct GetUserForUserSlugVariables<'a> {
    pub u_slug: &'a str,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "GetUserForUserSlugVariables")]
pub struct GetUserForUserSlug {
    #[arguments(slug: $u_slug)]
    pub user: Option<User>,
}

#[derive(cynic::QueryFragment, Debug)]
pub struct User {
    pub id: Option<cynic::Id>,
    pub name: Option<String>,
    pub player: Option<Player>,
}

#[derive(cynic::QueryFragment, Debug)]
pub struct Player {
    pub id: Option<cynic::Id>,
}

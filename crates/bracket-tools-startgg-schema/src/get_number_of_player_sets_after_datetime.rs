use crate::schema::schema;

#[derive(cynic::QueryVariables, Debug)]
pub struct GetNumberOfSetOfPlayerAfterDateVariables<'a> {
    pub player_slug: &'a str,
    pub updated_after: Timestamp,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "GetNumberOfSetOfPlayerAfterDateVariables")]
pub struct GetNumberOfSetOfPlayerAfterDate {
    #[arguments(slug: $player_slug)]
    pub user: Option<User>,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(variables = "GetNumberOfSetOfPlayerAfterDateVariables")]
pub struct User {
    pub player: Option<Player>,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(variables = "GetNumberOfSetOfPlayerAfterDateVariables")]
pub struct Player {
    #[arguments(page: 1, filters: { updatedAfter: $updated_after })]
    pub sets: Option<SetConnection>,
}

#[derive(cynic::QueryFragment, Debug)]
pub struct SetConnection {
    pub page_info: Option<PageInfo>,
}

#[derive(cynic::QueryFragment, Debug)]
pub struct PageInfo {
    pub total: Option<i32>,
}

#[derive(cynic::Scalar, Debug, Clone)]
pub struct Timestamp(pub String);

use crate::schema::schema;

#[derive(cynic::QueryVariables, Debug)]
pub struct GetSetsOfPlayerAfterDatetimeVariables<'a> {
    pub p_id: &'a cynic::Id,
    pub page_num: i32,
    pub results_per_page: i32,
    pub updated_after: Timestamp,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "GetSetsOfPlayerAfterDatetimeVariables")]
pub struct GetSetsOfPlayerAfterDatetime {
    #[arguments(id: $p_id)]
    pub player: Option<Player>,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(variables = "GetSetsOfPlayerAfterDatetimeVariables")]
pub struct Player {
    #[arguments(page: $page_num, perPage: $results_per_page, filters: { updatedAfter: $updated_after })]
    pub sets: Option<SetConnection>,
}

#[derive(cynic::QueryFragment, Debug)]
pub struct SetConnection {
    pub nodes: Option<Vec<Option<Set>>>,
}

#[derive(cynic::QueryFragment, Debug)]
pub struct Set {
    pub id: Option<cynic::Id>,
    pub completed_at: Option<Timestamp>,
    pub round: Option<i32>,
    pub games: Option<Vec<Option<Game>>>,
}

#[derive(cynic::QueryFragment, Debug)]
pub struct Game {
    pub winner_id: Option<i32>,
    pub selections: Option<Vec<Option<GameSelection>>>,
}

#[derive(cynic::QueryFragment, Debug)]
pub struct GameSelection {
    pub character: Option<Character>,
}

#[derive(cynic::QueryFragment, Debug)]
pub struct Character {
    pub id: Option<cynic::Id>,
}

#[derive(cynic::Scalar, Debug, Clone)]
pub struct Timestamp(pub String);

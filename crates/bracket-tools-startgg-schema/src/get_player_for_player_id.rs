use crate::schema::schema;

#[derive(cynic::QueryVariables, Debug)]
pub struct GetPlayerForPlayerIdVariables<'a> {
    pub p_id: &'a cynic::Id,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "GetPlayerForPlayerIdVariables")]
pub struct GetPlayerForPlayerId {
    #[arguments(id: $p_id)]
    pub player: Option<Player>,
}

#[derive(cynic::QueryFragment, Debug)]
pub struct Player {
    pub prefix: Option<String>,
    pub gamer_tag: Option<String>,
}

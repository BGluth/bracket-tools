use crate::schema::schema;

#[derive(cynic::QueryVariables, Debug)]
pub struct GetNumPlayersInTournamentVariables<'a> {
    pub t_id: &'a cynic::Id,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "GetNumPlayersInTournamentVariables")]
pub struct GetNumPlayersInTournament {
    #[arguments(id: $t_id)]
    pub tournament: Option<Tournament>,
}

#[derive(cynic::QueryFragment, Debug)]
pub struct Tournament {
    #[arguments(query: {  })]
    pub participants: Option<ParticipantConnection>,
}

#[derive(cynic::QueryFragment, Debug)]
pub struct ParticipantConnection {
    pub page_info: Option<PageInfo>,
}

#[derive(cynic::QueryFragment, Debug)]
pub struct PageInfo {
    pub total: Option<i32>,
}

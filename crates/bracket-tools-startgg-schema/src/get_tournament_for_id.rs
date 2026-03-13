use crate::schema::schema;

#[derive(cynic::QueryVariables, Debug)]
pub struct GetTournamentForIdVariables<'a> {
    pub num_per_page: i32,
    pub page_num: i32,
    pub t_id: &'a cynic::Id,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "GetTournamentForIdVariables")]
pub struct GetTournamentForId {
    #[arguments(id: $t_id)]
    pub tournament: Option<Tournament>,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(variables = "GetTournamentForIdVariables")]
pub struct Tournament {
    pub name: Option<String>,
    #[arguments(query: { page: $page_num, perPage: $num_per_page })]
    pub participants: Option<ParticipantConnection>,
}

#[derive(cynic::QueryFragment, Debug)]
pub struct ParticipantConnection {
    pub nodes: Option<Vec<Option<Participant>>>,
}

#[derive(cynic::QueryFragment, Debug)]
pub struct Participant {
    pub player: Option<Player>,
}

#[derive(cynic::QueryFragment, Debug)]
pub struct Player {
    pub id: Option<cynic::Id>,
}

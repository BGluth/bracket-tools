use crate::{
    enums::{ActivityState, BracketType},
    scalars::Timestamp,
    schema::schema,
};

#[derive(cynic::QueryVariables, Debug)]
pub struct GetEventStructureVariables<'a> {
    pub slug: &'a str,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "GetEventStructureVariables")]
pub struct GetEventStructure {
    #[arguments(slug: $slug)]
    pub event: Option<Event>,
}

#[derive(cynic::QueryFragment, Debug)]
pub struct Event {
    pub id: Option<cynic::Id>,
    pub name: Option<String>,
    pub state: Option<ActivityState>,
    pub start_at: Option<Timestamp>,
    pub tournament: Option<Tournament>,
    pub phases: Option<Vec<Option<Phase>>>,
    pub phase_groups: Option<Vec<Option<PhaseGroup>>>,
    pub num_entrants: Option<i32>,
}

#[derive(cynic::QueryFragment, Debug)]
pub struct Tournament {
    pub id: Option<cynic::Id>,
    pub slug: Option<String>,
}

#[derive(cynic::QueryFragment, Debug)]
pub struct Phase {
    pub id: Option<cynic::Id>,
    pub state: Option<ActivityState>,
}

#[derive(cynic::QueryFragment, Debug)]
pub struct PhaseGroup {
    pub id: Option<cynic::Id>,
    pub bracket_type: Option<BracketType>,
    pub num_rounds: Option<i32>,
    pub start_at: Option<Timestamp>,
    pub wave: Option<Wave>,
    pub rounds: Option<Vec<Option<Round>>>,
}

#[derive(cynic::QueryFragment, Debug)]
pub struct Wave {
    pub identifier: Option<String>,
    pub start_at: Option<Timestamp>,
}

#[derive(cynic::QueryFragment, Debug)]
pub struct Round {
    pub number: Option<i32>,
    pub best_of: Option<i32>,
}
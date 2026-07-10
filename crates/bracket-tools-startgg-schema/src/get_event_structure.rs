use crate::{
    enums::{ActivityState, BracketType},
    scalars::{Id, Timestamp},
    schema::schema,
};

#[derive(cynic::QueryVariables, Debug)]
pub struct GetEventStructureVariables<'a> {
    pub slug: &'a str,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "Query", variables = "GetEventStructureVariables")]
pub struct GetEventStructure {
    #[arguments(slug: $slug)]
    pub event: Option<Event>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct Event {
    pub id: Option<Id>,
    pub name: Option<String>,
    pub state: Option<ActivityState>,
    pub start_at: Option<Timestamp>,
    pub tournament: Option<Tournament>,
    pub phases: Option<Vec<Option<Phase>>>,
    pub phase_groups: Option<Vec<Option<PhaseGroup>>>,
    pub num_entrants: Option<i32>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct Tournament {
    pub id: Option<Id>,
    pub slug: Option<String>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct Phase {
    pub id: Option<Id>,
    pub state: Option<ActivityState>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct PhaseGroup {
    pub id: Option<Id>,
    pub bracket_type: Option<BracketType>,
    pub num_rounds: Option<i32>,
    pub start_at: Option<Timestamp>,
    pub wave: Option<Wave>,
    pub phase: Option<PhaseRef>,
    pub rounds: Option<Vec<Option<Round>>>,
}

/// The group's parent phase: groups sharing a phase run in parallel (pools);
/// `phase_order` sequences the phases (pools before the final bracket).
#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "Phase")]
pub struct PhaseRef {
    pub id: Option<Id>,
    pub phase_order: Option<i32>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct Wave {
    pub identifier: Option<String>,
    pub start_at: Option<Timestamp>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct Round {
    pub number: Option<i32>,
    pub best_of: Option<i32>,
}

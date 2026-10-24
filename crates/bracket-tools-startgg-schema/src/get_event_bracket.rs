//! An event's bracket structure for the admin tools: its phases with bracket
//! type, pool count, seed count and order, plus each phase's groups. Keyed by
//! event id (the roster query already maps event slugs to ids).

use crate::{
    enums::{ActivityState, BracketType},
    scalars::Id,
    schema::schema,
};

#[derive(cynic::QueryVariables, Debug)]
pub struct GetEventBracketVariables<'a> {
    pub event_id: &'a cynic::Id,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "Query", variables = "GetEventBracketVariables")]
pub struct GetEventBracket {
    #[arguments(id: $event_id)]
    pub event: Option<Event>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct Event {
    pub id: Option<Id>,
    pub name: Option<String>,
    pub slug: Option<String>,
    pub state: Option<ActivityState>,
    pub num_entrants: Option<i32>,
    pub phases: Option<Vec<Option<Phase>>>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct Phase {
    pub id: Option<Id>,
    pub name: Option<String>,
    pub bracket_type: Option<BracketType>,
    pub group_count: Option<i32>,
    pub num_seeds: Option<i32>,
    pub phase_order: Option<i32>,
    pub state: Option<ActivityState>,
    pub phase_groups: Option<PhaseGroupConnection>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct PhaseGroupConnection {
    pub nodes: Option<Vec<Option<PhaseGroup>>>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct PhaseGroup {
    pub id: Option<Id>,
    pub display_identifier: Option<String>,
}

#[cfg(test)]
mod tests {
    use cynic::QueryBuilder;

    use super::{GetEventBracket, GetEventBracketVariables};

    #[test]
    fn builds_a_query_operation() {
        let event_id = cynic::Id::new("1740963");
        let operation = GetEventBracket::build(GetEventBracketVariables { event_id: &event_id });

        assert!(operation.query.contains("phases"));
        assert!(operation.query.contains("bracketType"));
        assert!(operation.query.contains("groupCount"));
        assert!(operation.query.contains("phaseOrder"));
        assert!(operation.query.contains("displayIdentifier"));
    }
}

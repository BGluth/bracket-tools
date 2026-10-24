//! An event's paginated final standings: placement plus the entrant that
//! earned it. The selection surface for a redemption bracket ("everyone who
//! placed at or below N").

use crate::{entrant::Entrant, enums::ActivityState, scalars::Id, schema::schema};

#[derive(cynic::QueryVariables, Debug)]
pub struct GetEventStandingsVariables<'a> {
    pub event_id: &'a cynic::Id,
    pub page: i32,
    pub per_page: i32,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "Query", variables = "GetEventStandingsVariables")]
pub struct GetEventStandings {
    #[arguments(id: $event_id)]
    pub event: Option<Event>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(variables = "GetEventStandingsVariables")]
pub struct Event {
    pub id: Option<Id>,
    pub name: Option<String>,
    pub state: Option<ActivityState>,
    pub num_entrants: Option<i32>,
    #[arguments(query: { page: $page, perPage: $per_page })]
    pub standings: Option<StandingConnection>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct StandingConnection {
    pub page_info: Option<PageInfo>,
    pub nodes: Option<Vec<Option<Standing>>>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct PageInfo {
    pub total_pages: Option<i32>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct Standing {
    pub placement: Option<i32>,
    pub is_final: Option<bool>,
    pub entrant: Option<Entrant>,
}

#[cfg(test)]
mod tests {
    use cynic::QueryBuilder;

    use super::{GetEventStandings, GetEventStandingsVariables};

    #[test]
    fn builds_a_query_operation() {
        let event_id = cynic::Id::new("1740963");
        let operation = GetEventStandings::build(GetEventStandingsVariables {
            event_id: &event_id,
            page: 1,
            per_page: 100,
        });

        assert!(operation.query.contains("standings"));
        assert!(operation.query.contains("placement"));
        assert!(operation.query.contains("isFinal"));
        assert!(operation.query.contains("gamerTag"));
    }
}

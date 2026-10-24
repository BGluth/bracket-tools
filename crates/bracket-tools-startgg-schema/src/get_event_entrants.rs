//! An event's paginated entrant list. Unlike the tournament participant
//! roster, which is served from a search index that lags writes by minutes,
//! entrants read straight from the event — so this is how a registration is
//! verified right after the mutation.

use crate::{entrant::Entrant, scalars::Id, schema::schema};

#[derive(cynic::QueryVariables, Debug)]
pub struct GetEventEntrantsVariables<'a> {
    pub event_id: &'a cynic::Id,
    pub page: i32,
    pub per_page: i32,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "Query", variables = "GetEventEntrantsVariables")]
pub struct GetEventEntrants {
    #[arguments(id: $event_id)]
    pub event: Option<Event>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(variables = "GetEventEntrantsVariables")]
pub struct Event {
    pub id: Option<Id>,
    pub num_entrants: Option<i32>,
    #[arguments(query: { page: $page, perPage: $per_page })]
    pub entrants: Option<EntrantConnection>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct EntrantConnection {
    pub page_info: Option<PageInfo>,
    pub nodes: Option<Vec<Option<Entrant>>>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct PageInfo {
    pub total_pages: Option<i32>,
}

#[cfg(test)]
mod tests {
    use cynic::QueryBuilder;

    use super::{GetEventEntrants, GetEventEntrantsVariables};

    #[test]
    fn builds_a_query_operation() {
        let event_id = cynic::Id::new("1740963");
        let operation = GetEventEntrants::build(GetEventEntrantsVariables {
            event_id: &event_id,
            page: 1,
            per_page: 100,
        });

        assert!(operation.query.contains("entrants"));
        assert!(operation.query.contains("gamerTag"));
        assert!(operation.query.contains("totalPages"));
    }
}

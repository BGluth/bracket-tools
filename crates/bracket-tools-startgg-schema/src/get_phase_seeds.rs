//! A phase's paginated seed list: seed id and number plus the entrant behind
//! each seed. Seed ids are what `updatePhaseSeeding` takes, so this query is
//! the read half of the seeding flow.

use crate::{entrant::Entrant, scalars::Id, schema::schema};

#[derive(cynic::QueryVariables, Debug)]
pub struct GetPhaseSeedsVariables<'a> {
    pub phase_id: &'a cynic::Id,
    pub page: i32,
    pub per_page: i32,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "Query", variables = "GetPhaseSeedsVariables")]
pub struct GetPhaseSeeds {
    #[arguments(id: $phase_id)]
    pub phase: Option<Phase>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(variables = "GetPhaseSeedsVariables")]
pub struct Phase {
    pub id: Option<Id>,
    pub num_seeds: Option<i32>,
    #[arguments(query: { page: $page, perPage: $per_page })]
    pub seeds: Option<SeedConnection>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct SeedConnection {
    pub page_info: Option<PageInfo>,
    pub nodes: Option<Vec<Option<Seed>>>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct PageInfo {
    pub total_pages: Option<i32>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct Seed {
    pub id: Option<Id>,
    pub seed_num: Option<i32>,
    pub is_bye: Option<bool>,
    pub entrant: Option<Entrant>,
}

#[cfg(test)]
mod tests {
    use cynic::QueryBuilder;

    use super::{GetPhaseSeeds, GetPhaseSeedsVariables};

    #[test]
    fn builds_a_query_operation() {
        let phase_id = cynic::Id::new("2438095");
        let operation = GetPhaseSeeds::build(GetPhaseSeedsVariables {
            phase_id: &phase_id,
            page: 1,
            per_page: 100,
        });

        assert!(operation.query.contains("seeds"));
        assert!(operation.query.contains("seedNum"));
        assert!(operation.query.contains("gamerTag"));
        assert!(operation.query.contains("totalPages"));
    }
}

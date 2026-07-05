use crate::{scalars::Timestamp, schema::schema};

#[derive(cynic::QueryVariables, Debug)]
pub struct GetSetsForEventVariables<'a> {
    pub slug: &'a str,
    pub page: i32,
    pub per_page: i32,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "GetSetsForEventVariables")]
pub struct GetSetsForEvent {
    #[arguments(slug: $slug)]
    pub event: Option<Event>,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(variables = "GetSetsForEventVariables")]
pub struct Event {
    /// `hideEmpty: false` keeps not-yet-filled future sets in the response —
    /// their prereq edges are what lets a client assemble the bracket DAG.
    #[arguments(page: $page, perPage: $per_page, sortType: ROUND, filters: { hideEmpty: false })]
    pub sets: Option<SetConnection>,
}

#[derive(cynic::QueryFragment, Debug)]
pub struct SetConnection {
    pub page_info: Option<PageInfo>,
    pub nodes: Option<Vec<Option<Set>>>,
}

#[derive(cynic::QueryFragment, Debug)]
pub struct PageInfo {
    pub total_pages: Option<i32>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct Set {
    pub id: Option<cynic::Id>,
    pub state: Option<i32>,
    pub round: Option<i32>,
    pub identifier: Option<String>,
    pub full_round_text: Option<String>,
    pub started_at: Option<Timestamp>,
    pub completed_at: Option<Timestamp>,
    pub winner_id: Option<i32>,
    pub has_placeholder: Option<bool>,
    pub phase_group: Option<PhaseGroup>,
    pub slots: Option<Vec<Option<SetSlot>>>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct PhaseGroup {
    pub id: Option<cynic::Id>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct SetSlot {
    pub slot_index: Option<i32>,
    pub prereq_id: Option<String>,
    pub prereq_type: Option<String>,
    pub prereq_placement: Option<i32>,
    pub entrant: Option<Entrant>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct Entrant {
    pub id: Option<cynic::Id>,
    pub name: Option<String>,
    pub is_disqualified: Option<bool>,
    pub participants: Option<Vec<Option<Participant>>>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct Participant {
    pub gamer_tag: Option<String>,
    pub player: Option<Player>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct Player {
    pub id: Option<cynic::Id>,
}

#[cfg(test)]
mod tests {
    use cynic::QueryBuilder;

    use super::{GetSetsForEvent, GetSetsForEventVariables};

    #[test]
    fn query_carries_sort_filter_and_pagination_arguments() {
        let operation = GetSetsForEvent::build(GetSetsForEventVariables {
            slug: "tournament/x/event/y",
            page: 1,
            per_page: 50,
        });

        assert!(operation.query.contains("ROUND"));
        assert!(operation.query.contains("hideEmpty"));
        assert!(operation.query.contains("totalPages"));
    }
}

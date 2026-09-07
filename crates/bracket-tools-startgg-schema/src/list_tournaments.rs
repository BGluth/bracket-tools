//! Filtered tournament listing: the `tournaments(query)` connection driven by
//! a structured `TournamentPageFilter` (region, date window, games, owner,
//! admin-of) and the fields a tournament card needs — dates, venue,
//! registration state, and the event list with each event's game. Sorted by
//! `startAt` ascending; null filter halves are omitted server-side.

use crate::{
    scalars::{Id, Timestamp},
    schema::schema,
};

#[derive(cynic::QueryVariables, Debug)]
pub struct ListTournamentsVariables {
    pub filter: TournamentPageFilter,
    pub page: i32,
    pub per_page: i32,
}

#[derive(cynic::InputObject, Debug, Clone, Default)]
pub struct TournamentPageFilter {
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub owner_id: Option<cynic::Id>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub is_current_user_admin: Option<bool>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub country_code: Option<String>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub addr_state: Option<String>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub location: Option<TournamentLocationFilter>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub after_date: Option<Timestamp>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub before_date: Option<Timestamp>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub published: Option<bool>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub videogame_ids: Option<Vec<Option<cynic::Id>>>,
}

/// A radius search: `distance_from` is `"lat,lng"`, `distance` is e.g. `"50mi"`.
#[derive(cynic::InputObject, Debug, Clone)]
pub struct TournamentLocationFilter {
    pub distance_from: Option<String>,
    pub distance: Option<String>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "Query", variables = "ListTournamentsVariables")]
pub struct ListTournaments {
    #[arguments(query: { page: $page, perPage: $per_page, sortBy: "startAt asc", filter: $filter })]
    pub tournaments: Option<TournamentConnection>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct TournamentConnection {
    pub page_info: Option<PageInfo>,
    pub nodes: Option<Vec<Option<Tournament>>>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct PageInfo {
    pub total_pages: Option<i32>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct Tournament {
    pub id: Option<Id>,
    pub name: Option<String>,
    pub slug: Option<String>,
    pub start_at: Option<Timestamp>,
    pub end_at: Option<Timestamp>,
    pub created_at: Option<Timestamp>,
    pub updated_at: Option<Timestamp>,
    pub city: Option<String>,
    pub addr_state: Option<String>,
    pub country_code: Option<String>,
    pub venue_name: Option<String>,
    pub venue_address: Option<String>,
    pub timezone: Option<String>,
    pub is_registration_open: Option<bool>,
    pub registration_closes_at: Option<Timestamp>,
    pub num_attendees: Option<i32>,
    pub owner: Option<User>,
    pub events: Option<Vec<Option<Event>>>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct User {
    pub id: Option<Id>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct Event {
    pub id: Option<Id>,
    pub name: Option<String>,
    pub videogame: Option<Videogame>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct Videogame {
    pub id: Option<Id>,
    pub name: Option<String>,
}

#[cfg(test)]
mod tests {
    use cynic::QueryBuilder;

    use super::{ListTournaments, ListTournamentsVariables, TournamentLocationFilter, TournamentPageFilter};
    use crate::scalars::Timestamp;

    #[test]
    fn builds_a_query_operation() {
        let operation = ListTournaments::build(ListTournamentsVariables {
            filter: TournamentPageFilter {
                owner_id: Some(cynic::Id::new("123")),
                is_current_user_admin: None,
                country_code: Some("CA".into()),
                addr_state: Some("AB".into()),
                location: Some(TournamentLocationFilter {
                    distance_from: Some("53.5,-113.5".into()),
                    distance: Some("50mi".into()),
                }),
                after_date: Some(Timestamp(1_700_000_000)),
                before_date: None,
                published: Some(true),
                videogame_ids: Some(vec![Some(cynic::Id::new("1386"))]),
            },
            page: 1,
            per_page: 50,
        });

        assert!(operation.query.contains("tournaments"));
        assert!(operation.query.contains("startAt asc"));
        assert!(operation.query.contains("totalPages"));
        assert!(operation.query.contains("videogame"));
    }

    #[test]
    fn unset_filter_halves_are_omitted() {
        let operation = ListTournaments::build(ListTournamentsVariables {
            filter: TournamentPageFilter {
                addr_state: Some("AB".into()),
                ..TournamentPageFilter::default()
            },
            page: 1,
            per_page: 50,
        });
        let variables = serde_json::to_string(&operation.variables).unwrap();

        assert!(variables.contains("addrState"));
        assert!(!variables.contains("ownerId"));
        assert!(!variables.contains("location"));
    }
}

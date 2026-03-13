use crate::schema::schema;

#[derive(cynic::QueryVariables, Debug)]
pub struct GetEventIdVariables<'a> {
    pub slug: Option<&'a str>,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "GetEventIdVariables")]
pub struct GetEventId {
    #[arguments(slug: $slug)]
    pub event: Option<Event>,
}

#[derive(cynic::QueryFragment, Debug)]
pub struct Event {
    pub id: Option<cynic::Id>,
    pub name: Option<String>,
}

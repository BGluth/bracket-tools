//! An event's videogame and its character roster — the vocabulary for
//! reporting per-game character selections.

use crate::{scalars::Id, schema::schema};

#[derive(cynic::QueryVariables, Debug)]
pub struct GetEventCharactersVariables<'a> {
    pub slug: &'a str,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
#[cynic(graphql_type = "Query", variables = "GetEventCharactersVariables")]
pub struct GetEventCharacters {
    #[arguments(slug: $slug)]
    pub event: Option<Event>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct Event {
    pub videogame: Option<Videogame>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct Videogame {
    pub id: Option<Id>,
    pub name: Option<String>,
    pub characters: Option<Vec<Option<Character>>>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct Character {
    pub id: Option<Id>,
    pub name: Option<String>,
}

#[cfg(test)]
mod tests {
    use cynic::QueryBuilder;

    use super::{GetEventCharacters, GetEventCharactersVariables};

    #[test]
    fn builds_a_query_operation() {
        let operation = GetEventCharacters::build(GetEventCharactersVariables {
            slug: "tournament/t/event/e",
        });

        assert!(operation.query.contains("videogame"));
        assert!(operation.query.contains("characters"));
    }
}

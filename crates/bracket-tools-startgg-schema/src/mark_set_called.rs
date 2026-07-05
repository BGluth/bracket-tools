use crate::{scalars::Timestamp, schema::schema};

#[derive(cynic::QueryVariables, Debug)]
pub struct MarkSetCalledVariables<'a> {
    pub set_id: &'a cynic::Id,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Mutation", variables = "MarkSetCalledVariables")]
pub struct MarkSetCalled {
    #[arguments(setId: $set_id)]
    pub mark_set_called: Option<Set>,
}

/// The mutation's return payload. `state` is start.gg's undocumented Int;
/// values observed here are recorded evidence for the scheduler's state map.
#[derive(cynic::QueryFragment, Debug)]
pub struct Set {
    pub id: Option<cynic::Id>,
    pub state: Option<i32>,
    pub started_at: Option<Timestamp>,
    pub completed_at: Option<Timestamp>,
}

#[cfg(test)]
mod tests {
    use cynic::MutationBuilder;

    use super::{MarkSetCalled, MarkSetCalledVariables};

    #[test]
    fn builds_a_mutation_operation() {
        let set_id = cynic::Id::new("12345");
        let operation = MarkSetCalled::build(MarkSetCalledVariables { set_id: &set_id });

        assert!(operation.query.starts_with("mutation"));
        assert!(operation.query.contains("markSetCalled"));
    }
}

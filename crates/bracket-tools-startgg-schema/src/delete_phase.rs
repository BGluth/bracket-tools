//! Deletes a phase by id (e.g. a leftover "Top 8" after collapsing an event
//! into one bracket). start.gg answers with a bare success flag.

use crate::schema::schema;

#[derive(cynic::QueryVariables, Debug)]
pub struct DeletePhaseVariables<'a> {
    pub phase_id: &'a cynic::Id,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Mutation", variables = "DeletePhaseVariables")]
pub struct DeletePhase {
    #[arguments(phaseId: $phase_id)]
    pub delete_phase: Option<bool>,
}

#[cfg(test)]
mod tests {
    use cynic::MutationBuilder;

    use super::{DeletePhase, DeletePhaseVariables};

    #[test]
    fn builds_a_mutation_operation() {
        let phase_id = cynic::Id::new("2438096");
        let operation = DeletePhase::build(DeletePhaseVariables { phase_id: &phase_id });

        assert!(operation.query.starts_with("mutation"));
        assert!(operation.query.contains("deletePhase"));
    }
}

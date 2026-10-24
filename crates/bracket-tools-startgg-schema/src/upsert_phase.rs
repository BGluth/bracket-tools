//! Create or reshape a phase: with `phaseId` it edits that phase, with
//! `eventId` alone it creates a new one. The payload carries only the fields
//! being set (name, bracket type, pool count).

use crate::{enums::BracketType, scalars::Id, schema::schema};

#[derive(cynic::QueryVariables, Debug)]
pub struct UpsertPhaseVariables {
    pub phase_id: Option<cynic::Id>,
    pub event_id: Option<cynic::Id>,
    pub payload: PhaseUpsertInput,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Mutation", variables = "UpsertPhaseVariables")]
pub struct UpsertPhase {
    #[arguments(phaseId: $phase_id, eventId: $event_id, payload: $payload)]
    pub upsert_phase: Option<Phase>,
}

#[derive(cynic::QueryFragment, Debug)]
pub struct Phase {
    pub id: Option<Id>,
    pub name: Option<String>,
    pub bracket_type: Option<BracketType>,
    pub group_count: Option<i32>,
    pub num_seeds: Option<i32>,
    pub phase_order: Option<i32>,
}

#[derive(cynic::InputObject, Debug, Clone, Default)]
pub struct PhaseUpsertInput {
    pub name: Option<String>,
    pub group_count: Option<i32>,
    pub bracket_type: Option<BracketType>,
}

#[cfg(test)]
mod tests {
    use cynic::MutationBuilder;

    use super::{BracketType, PhaseUpsertInput, UpsertPhase, UpsertPhaseVariables};

    #[test]
    fn builds_a_mutation_operation() {
        let operation = UpsertPhase::build(UpsertPhaseVariables {
            phase_id: Some(cynic::Id::new("2438095")),
            event_id: None,
            payload: PhaseUpsertInput {
                name: None,
                group_count: Some(1),
                bracket_type: Some(BracketType::DoubleElimination),
            },
        });

        assert!(operation.query.starts_with("mutation"));
        assert!(operation.query.contains("upsertPhase"));

        let variables = serde_json::to_string(&operation.variables).unwrap();
        assert!(variables.contains("DOUBLE_ELIMINATION"));
        assert!(variables.contains("groupCount"));
    }
}

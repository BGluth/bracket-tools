//! Rewrites a phase's seeding: every seed id paired with its new seed number.
//! `seedNum` is typed `ID` by start.gg (an opaque string holding the number).
//! Strict mode makes the server reject a mapping that doesn't cover every
//! entrant in the phase.

use crate::{scalars::Id, schema::schema};

#[derive(cynic::QueryVariables, Debug)]
pub struct UpdatePhaseSeedingVariables<'a> {
    pub phase_id: &'a cynic::Id,
    pub seed_mapping: Vec<Option<UpdatePhaseSeedInfo>>,
    pub options: Option<UpdatePhaseSeedingOptions>,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Mutation", variables = "UpdatePhaseSeedingVariables")]
pub struct UpdatePhaseSeeding {
    #[arguments(phaseId: $phase_id, seedMapping: $seed_mapping, options: $options)]
    pub update_phase_seeding: Option<Phase>,
}

#[derive(cynic::QueryFragment, Debug)]
pub struct Phase {
    pub id: Option<Id>,
    pub num_seeds: Option<i32>,
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct UpdatePhaseSeedInfo {
    pub seed_id: cynic::Id,
    pub seed_num: cynic::Id,
    pub phase_group_id: Option<cynic::Id>,
}

#[derive(cynic::InputObject, Debug, Clone)]
pub struct UpdatePhaseSeedingOptions {
    pub strict_mode: Option<bool>,
}

#[cfg(test)]
mod tests {
    use cynic::MutationBuilder;

    use super::{UpdatePhaseSeedInfo, UpdatePhaseSeeding, UpdatePhaseSeedingOptions, UpdatePhaseSeedingVariables};

    #[test]
    fn builds_a_mutation_operation() {
        let phase_id = cynic::Id::new("2438095");
        let operation = UpdatePhaseSeeding::build(UpdatePhaseSeedingVariables {
            phase_id: &phase_id,
            seed_mapping: vec![Some(UpdatePhaseSeedInfo {
                seed_id: cynic::Id::new("777"),
                seed_num: cynic::Id::new("1"),
                phase_group_id: None,
            })],
            options: Some(UpdatePhaseSeedingOptions { strict_mode: Some(true) }),
        });

        assert!(operation.query.starts_with("mutation"));
        assert!(operation.query.contains("updatePhaseSeeding"));

        let variables = serde_json::to_string(&operation.variables).unwrap();
        assert!(variables.contains("seedMapping"));
        assert!(variables.contains("strictMode"));
    }
}

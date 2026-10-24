//! The entrant fragment shared by the bracket-admin queries (phase seeds,
//! event standings): the entrant's identity plus each participant's tag and
//! user id — the handles the seeding and registration flows match on.

use crate::{scalars::Id, schema::schema};

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct Entrant {
    pub id: Option<Id>,
    pub name: Option<String>,
    pub participants: Option<Vec<Option<Participant>>>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct Participant {
    pub gamer_tag: Option<String>,
    pub prefix: Option<String>,
    pub user: Option<User>,
}

#[derive(cynic::QueryFragment, Debug, Clone)]
pub struct User {
    pub id: Option<Id>,
}

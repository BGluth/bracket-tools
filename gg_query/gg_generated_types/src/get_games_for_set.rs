use crate::schema::schema;

#[derive(cynic::QueryVariables, Debug)]
pub struct GetGamesOfSetVariables<'a> {
    pub s_id: &'a cynic::Id,
}

#[derive(cynic::QueryFragment, Debug)]
#[cynic(graphql_type = "Query", variables = "GetGamesOfSetVariables")]
pub struct GetGamesOfSet {
    #[arguments(id: $s_id)]
    pub set: Option<Set>,
}

#[derive(cynic::QueryFragment, Debug)]
pub struct Set {
    pub games: Option<Vec<Option<Game>>>,
    #[arguments(includeByes: true)]
    pub slots: Option<Vec<Option<SetSlot>>>,
}

#[derive(cynic::QueryFragment, Debug)]
pub struct SetSlot {
    pub standing: Option<Standing>,
}

#[derive(cynic::QueryFragment, Debug)]
pub struct Standing {
    pub entrant: Option<Entrant>,
    pub stats: Option<StandingStats>,
}

#[derive(cynic::QueryFragment, Debug)]
pub struct StandingStats {
    pub score: Option<Score>,
}

#[derive(cynic::QueryFragment, Debug)]
pub struct Score {
    pub value: Option<f64>,
}

#[derive(cynic::QueryFragment, Debug)]
pub struct Game {
    pub winner_id: Option<i32>,
    pub selections: Option<Vec<Option<GameSelection>>>,
}

#[derive(cynic::QueryFragment, Debug)]
pub struct GameSelection {
    pub character: Option<Character>,
}

#[derive(cynic::QueryFragment, Debug)]
pub struct Entrant {
    pub participants: Option<Vec<Option<Participant>>>,
}

#[derive(cynic::QueryFragment, Debug)]
pub struct Participant {
    pub player: Option<Player>,
}

#[derive(cynic::QueryFragment, Debug)]
pub struct Player {
    pub id: Option<cynic::Id>,
}

#[derive(cynic::QueryFragment, Debug)]
pub struct Character {
    pub id: Option<cynic::Id>,
}

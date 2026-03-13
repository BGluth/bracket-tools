use chrono::Duration;

use super::store_utils::Store;
use crate::types::{DateTime, Provider, RollingAverage};

#[derive(Copy, Clone, Debug)]
pub enum TourneyStoreAction {}

#[derive(Clone, Debug)]
pub struct StatsStore {
    pub name: String,
    pub event_stats: Vec<EventStats>,
    pub time_stats: TimeStats,
}

impl StatsStore {
    pub(super) async fn new<P: Provider>(provider: &P) -> Self {
        todo!()
    }
}

impl Store for StatsStore {
    type Action = TourneyStoreAction;

    fn update(&mut self, action: Self::Action) {
        match action {}
    }
}

#[derive(Clone, Debug)]
struct EventStats {
    pub p_stats: RegedPlayerStats,
    pub g_stats: GameStats,
}

#[derive(Clone, Debug)]
struct RegedPlayerStats {
    pub num_reged: usize,

    /// The number of players that have checked-in and completed any other pre-reqs (eg. have payed).
    pub num_checked_in: usize,
}

#[derive(Clone, Debug)]
struct GameStats {
    pub num_games: usize,
    pub num_completed: usize,
}

#[derive(Clone, Debug)]
struct TimeStats {
    pub start_time: DateTime,
    pub avg_set_duration: RollingAverage<f32>,
}

impl TimeStats {
    fn process_completed_set_dur(&mut self, dur: Duration) {
        todo!()
    }

    fn get_estimated_tourney_completion_time(&self) -> DateTime {
        todo!()
    }
}

#[derive(Debug)]
struct ApiStats {
    pub avg_resp_time: RollingAverage<f32>,
    pub complexity_buf: QueryComplexityBuffer,
}

#[derive(Debug)]
struct QueryComplexityBuffer {
    pub curr_complexity: usize,
    pub backoff_complexity: usize,
}

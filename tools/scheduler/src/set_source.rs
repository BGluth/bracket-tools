use std::{error::Error, future::Future};

use bracket_tools_cache::null_storage::NullStorage;
use bracket_tools_startgg::{AdminProbeResult, CharacterInfo, GGProvider, GGProviderError, GameReport, SetMutationResult, StartGgId};
use bracket_tools_startgg_schema::{get_event_structure, get_sets_for_event};

/// A source of live bracket data the scheduler polls and writes through.
///
/// The scheduler is generic over this trait so a fixture-replay source can
/// stand in for start.gg in tests and `--simulate` runs.
///
/// Declared in the desugared `impl Future` form (like `Storage`); impls can
/// use plain `async fn`. The `+ Send` bounds exist for the *generic* task
/// wiring (`tokio::spawn` inside `run<S: SetSource>`): monomorphic spawns
/// proved Send without them (the S1 spike), but generic code can't see
/// through an opaque RPITIT, so the trait states it. Both implementations'
/// futures are naturally Send.
pub trait SetSource {
    type Error: Error + Send + Sync + 'static;

    /// Fetches every set in an event, including not-yet-filled future sets.
    fn fetch_event_sets(&self, event_slug: &str) -> impl Future<Output = Result<Vec<get_sets_for_event::Set>, Self::Error>> + Send;

    /// Fetches an event's structural skeleton (phases, groups, waves, rounds).
    fn fetch_event_structure(&self, event_slug: &str) -> impl Future<Output = Result<get_event_structure::Event, Self::Error>> + Send;

    /// Marks a set as called (players summoned to their station).
    fn mark_called(&self, set_id: StartGgId) -> impl Future<Output = Result<SetMutationResult, Self::Error>> + Send;

    /// Marks a set as in progress.
    fn mark_in_progress(&self, set_id: StartGgId) -> impl Future<Output = Result<SetMutationResult, Self::Error>> + Send;

    /// Probes whether the token administers the tournament (preflight's
    /// writes-armed decision).
    fn probe_admin(&self, tournament_id: StartGgId) -> impl Future<Output = Result<AdminProbeResult, Self::Error>> + Send;

    /// Fetches an event's videogame character roster (empty when the event
    /// has no character data).
    fn fetch_event_characters(&self, event_slug: &str) -> impl Future<Output = Result<Vec<CharacterInfo>, Self::Error>> + Send;

    /// Reports a set's result: winner, optional per-game data, DQ flag.
    fn report_set(
        &self,
        set_id: StartGgId,
        winner_entrant_id: Option<String>,
        is_dq: bool,
        games: Vec<GameReport>,
    ) -> impl Future<Output = Result<SetMutationResult, Self::Error>> + Send;
}

/// A [`SetSource`] backed by the live start.gg API through an uncached
/// provider — the scheduler wants a full fresh snapshot every poll.
pub struct StartggSource {
    provider: GGProvider<NullStorage>,
}

impl StartggSource {
    pub fn new(provider: GGProvider<NullStorage>) -> Self {
        Self { provider }
    }
}

impl SetSource for StartggSource {
    type Error = GGProviderError;

    async fn fetch_event_sets(&self, event_slug: &str) -> Result<Vec<get_sets_for_event::Set>, Self::Error> {
        self.provider.fetch_event_sets(event_slug).await
    }

    async fn fetch_event_structure(&self, event_slug: &str) -> Result<get_event_structure::Event, Self::Error> {
        self.provider.fetch_event_structure(event_slug).await
    }

    async fn mark_called(&self, set_id: StartGgId) -> Result<SetMutationResult, Self::Error> {
        self.provider.mark_set_called(set_id).await
    }

    async fn mark_in_progress(&self, set_id: StartGgId) -> Result<SetMutationResult, Self::Error> {
        self.provider.mark_set_in_progress(set_id).await
    }

    async fn probe_admin(&self, tournament_id: StartGgId) -> Result<AdminProbeResult, Self::Error> {
        self.provider.fetch_admin_probe(tournament_id).await
    }

    async fn fetch_event_characters(&self, event_slug: &str) -> Result<Vec<CharacterInfo>, Self::Error> {
        self.provider.fetch_event_characters(event_slug).await
    }

    async fn report_set(
        &self,
        set_id: StartGgId,
        winner_entrant_id: Option<String>,
        is_dq: bool,
        games: Vec<GameReport>,
    ) -> Result<SetMutationResult, Self::Error> {
        self.provider
            .report_bracket_set(set_id, winner_entrant_id.as_deref(), is_dq, &games)
            .await
    }
}

// The real poll loop lives in `crate::poller` (S3); its Send spike over this
// source is `poller::tests::live_poller_future_is_send`.

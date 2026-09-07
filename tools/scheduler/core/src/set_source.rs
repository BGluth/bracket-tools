use std::{error::Error, future::Future};

use bracket_tools_cache::null_storage::NullStorage;
use bracket_tools_startgg::{AdminProbeResult, CharacterInfo, GGProvider, GGProviderError, GameReport, SetMutationResult, StartGgId};
use bracket_tools_startgg_schema::{get_event_structure, get_sets_for_event};
use cfg_if::cfg_if;

cfg_if! {
    if #[cfg(target_arch = "wasm32")] {
        /// Unbounded on wasm32, where browser futures are `!Send`; `Send` natively.
        pub trait MaybeSend {}
        impl<T> MaybeSend for T {}
        /// Unbounded on wasm32; `Sync` natively.
        pub trait MaybeSync {}
        impl<T> MaybeSync for T {}
    } else {
        /// `Send` natively, where generic task spawning must see it through the
        /// opaque RPITIT; unbounded on wasm32, where browser futures are `!Send`.
        pub trait MaybeSend: Send {}
        impl<T: Send> MaybeSend for T {}
        /// `Sync` natively; unbounded on wasm32 (see [`MaybeSend`]).
        pub trait MaybeSync: Sync {}
        impl<T: Sync> MaybeSync for T {}
    }
}

/// A source of live bracket data the scheduler polls and writes through.
///
/// The scheduler is generic over this trait so a fixture-replay source can
/// stand in for start.gg in tests and `--simulate` runs.
///
/// Declared in the desugared `impl Future` form (like `Storage`); impls can
/// use plain `async fn`. The [`MaybeSend`] bounds let generic task wiring
/// (`tokio::spawn` inside `run<S: SetSource>`) see through the opaque RPITIT
/// natively while the same trait compiles for the browser.
pub trait SetSource {
    type Error: Error + MaybeSend + MaybeSync + 'static;

    /// Fetches every set in an event, including not-yet-filled future sets.
    fn fetch_event_sets(&self, event_slug: &str) -> impl Future<Output = Result<Vec<get_sets_for_event::Set>, Self::Error>> + MaybeSend;

    /// Fetches an event's structural skeleton (phases, groups, waves, rounds).
    fn fetch_event_structure(&self, event_slug: &str) -> impl Future<Output = Result<get_event_structure::Event, Self::Error>> + MaybeSend;

    /// Marks a set as called (players summoned to their station).
    fn mark_called(&self, set_id: StartGgId) -> impl Future<Output = Result<SetMutationResult, Self::Error>> + MaybeSend;

    /// Marks a set as in progress.
    fn mark_in_progress(&self, set_id: StartGgId) -> impl Future<Output = Result<SetMutationResult, Self::Error>> + MaybeSend;

    /// Probes whether the token administers the tournament (preflight's
    /// writes-armed decision).
    fn probe_admin(&self, tournament_id: StartGgId) -> impl Future<Output = Result<AdminProbeResult, Self::Error>> + MaybeSend;

    /// Fetches an event's videogame character roster (empty when the event
    /// has no character data).
    fn fetch_event_characters(&self, event_slug: &str) -> impl Future<Output = Result<Vec<CharacterInfo>, Self::Error>> + MaybeSend;

    /// Reports a set's result: winner, optional per-game data, DQ flag.
    fn report_set(
        &self,
        set_id: StartGgId,
        winner_entrant_id: Option<String>,
        is_dq: bool,
        games: Vec<GameReport>,
    ) -> impl Future<Output = Result<SetMutationResult, Self::Error>> + MaybeSend;
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

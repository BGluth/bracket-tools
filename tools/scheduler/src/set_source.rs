use std::{error::Error, future::Future};

use bracket_tools_cache::null_storage::NullStorage;
use bracket_tools_startgg::{GGProvider, GGProviderError, SetMutationResult, StartGgId};
use bracket_tools_startgg_schema::{get_event_structure, get_sets_for_event};

/// A source of live bracket data the scheduler polls and writes through.
///
/// The scheduler is generic over this trait so a fixture-replay source can
/// stand in for start.gg in tests and `--simulate` runs. Methods return
/// schema-layer types in S1; the scheduler-local set model arrives in S3.
///
/// Declared in the desugared `impl Future` form (like `Storage`); impls can
/// use plain `async fn`.
pub trait SetSource {
    type Error: Error + Send + Sync + 'static;

    /// Fetches every set in an event, including not-yet-filled future sets.
    fn fetch_event_sets(&self, event_slug: &str) -> impl Future<Output = Result<Vec<get_sets_for_event::Set>, Self::Error>>;

    /// Fetches an event's structural skeleton (phases, groups, waves, rounds).
    fn fetch_event_structure(&self, event_slug: &str) -> impl Future<Output = Result<get_event_structure::Event, Self::Error>>;

    /// Marks a set as called (players summoned to their station).
    fn mark_called(&self, set_id: StartGgId) -> impl Future<Output = Result<SetMutationResult, Self::Error>>;

    /// Marks a set as in progress.
    fn mark_in_progress(&self, set_id: StartGgId) -> impl Future<Output = Result<SetMutationResult, Self::Error>>;
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
}

// The real poll loop lives in `crate::poller` (S3); its Send spike over this
// source is `poller::tests::live_poller_future_is_send`.

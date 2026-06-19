use std::{collections::HashMap, future::Future, sync::Arc};

use bracket_tools_cache::storage::Storage;
use parking_lot::Mutex;
use tokio::sync::OnceCell;

use crate::{
    gg_data_types::{HydratedGgPlayer, HydratedGgSet, HydratedGgTournament, StartGgId},
    lazy::{LazyPlayer, LazySet, LazyTournament},
    provider::{GGProvider, GGProviderError},
};

/// A per-id, in-memory tier of fully-deserialized values.
///
/// Each id maps to its own [`OnceCell`], so concurrent misses for the same id
/// coalesce into a single underlying fetch and every handle observing that id
/// shares one `Arc<T>`.
type IdCache<T> = Mutex<HashMap<StartGgId, Arc<OnceCell<Arc<T>>>>>;

/// Owns a [`GGProvider`] and mints lightweight [`LazyTournament`]/[`LazySet`]/
/// [`LazyPlayer`] handles that share its HTTP client, rate limiter, and cache.
///
/// On top of the provider's sled tier, the session adds an in-memory tier of
/// fully-deserialized `Arc<HydratedGg*>` values. Handles cloned from the same
/// session and pointing at the same id resolve to the same `Arc` — cross-handle
/// dedup and request-coalescing for free. Entries live for the session's
/// lifetime (no TTL/eviction in this version).
pub struct GgSession<S: Storage> {
    provider: GGProvider<S>,
    tournaments: IdCache<HydratedGgTournament>,
    players: IdCache<HydratedGgPlayer>,
    sets: IdCache<HydratedGgSet>,
}

impl<S: Storage> GgSession<S> {
    /// Wraps a provider in a shared session with empty in-memory caches.
    pub fn new(provider: GGProvider<S>) -> Arc<Self> {
        Arc::new(Self {
            provider,
            tournaments: Mutex::new(HashMap::new()),
            players: Mutex::new(HashMap::new()),
            sets: Mutex::new(HashMap::new()),
        })
    }

    /// Mints a tournament handle. No I/O — the fetch happens on `.get()`.
    pub fn tournament(self: &Arc<Self>, id: StartGgId) -> LazyTournament<S> {
        LazyTournament::new(id, Arc::clone(self))
    }

    /// Mints a player handle. No I/O — the fetch happens on `.get()`.
    pub fn player(self: &Arc<Self>, id: StartGgId) -> LazyPlayer<S> {
        LazyPlayer::new(id, Arc::clone(self))
    }

    /// Mints a set handle. No I/O — the fetch happens on `.get()`.
    pub fn set(self: &Arc<Self>, id: StartGgId) -> LazySet<S> {
        LazySet::new(id, Arc::clone(self))
    }

    pub(crate) async fn tournament_value(&self, id: StartGgId) -> Result<Arc<HydratedGgTournament>, GGProviderError> {
        get_or_fetch(&self.tournaments, id, || self.provider.get_tournament(id)).await
    }

    pub(crate) async fn player_value(&self, id: StartGgId) -> Result<Arc<HydratedGgPlayer>, GGProviderError> {
        get_or_fetch(&self.players, id, || self.provider.get_player(id)).await
    }

    pub(crate) async fn set_value(&self, id: StartGgId) -> Result<Arc<HydratedGgSet>, GGProviderError> {
        get_or_fetch(&self.sets, id, || self.provider.get_set_games(id)).await
    }
}

/// Returns the memoized `Arc<T>` for `id`, fetching once on the first miss.
///
/// The map lock is held only long enough to clone out (or insert) the id's
/// `OnceCell`; the fetch runs without the lock, and the cell guarantees a single
/// initialization shared across concurrent callers.
async fn get_or_fetch<T, F, Fut>(map: &IdCache<T>, id: StartGgId, fetch: F) -> Result<Arc<T>, GGProviderError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T, GGProviderError>>,
{
    let cell = map.lock().entry(id).or_insert_with(|| Arc::new(OnceCell::new())).clone();

    let value = cell.get_or_try_init(|| async { fetch().await.map(Arc::new) }).await?;

    Ok(Arc::clone(value))
}

#[cfg(test)]
mod tests {
    use std::{str::FromStr, sync::Arc, time::SystemTime};

    use bracket_tools_cache::{sled_storage::SledStorage, storage::Storage};
    use serde::Serialize;

    use super::GgSession;
    use crate::{
        gg_data_types::{HydratedGgPlayer, HydratedGgSet, HydratedGgTournament, Matchup, SlotData, StartGgId},
        provider::{cache_key, GGProvider},
        types::GGRestToken,
    };

    fn test_token() -> GGRestToken {
        GGRestToken::from_str("91b0c4b4aeae0a040d5b2c0e4d8861c2").unwrap()
    }

    /// Builds a session over a (possibly pre-populated) sled store with a dummy
    /// token. Any value not already cached would trigger a failing HTTP call, so
    /// a successful `.get()` proves the value was served from a cache tier.
    fn session_over(storage: SledStorage) -> Arc<GgSession<SledStorage>> {
        let provider = GGProvider::builder_with_storage(test_token(), storage).build().unwrap();
        GgSession::new(provider)
    }

    async fn seed<T: Serialize>(storage: &SledStorage, entity: &str, id: StartGgId, value: &T) {
        let bytes = bincode::serialize(value).unwrap();
        storage.put(&cache_key(entity, id), SystemTime::now(), &bytes).await.unwrap();
    }

    fn player(id: StartGgId, gamer_tag: &str) -> HydratedGgPlayer {
        HydratedGgPlayer {
            id,
            gamer_tag: gamer_tag.to_string(),
            prefix: None,
        }
    }

    #[tokio::test]
    async fn minting_a_handle_performs_no_fetch() {
        // Dummy token + empty cache: only never calling `.get()` keeps this green.
        let session = session_over(SledStorage::builder().build().unwrap());
        let handle = session.player(42);
        assert_eq!(handle.id(), 42);
    }

    #[tokio::test]
    async fn handles_for_the_same_id_share_one_value() {
        let storage = SledStorage::builder().build().unwrap();
        let expected = player(42, "Mang0");
        seed(&storage, "player", 42, &expected).await;
        let session = session_over(storage);

        let first = session.player(42).get().await.unwrap();
        let second = session.player(42).get().await.unwrap();

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(*first, expected);
    }

    #[tokio::test]
    async fn concurrent_gets_coalesce_to_one_value() {
        let storage = SledStorage::builder().build().unwrap();
        seed(&storage, "player", 7, &player(7, "Zain")).await;
        let session = session_over(storage);

        let first_handle = session.player(7);
        let second_handle = session.player(7);
        let (first, second) = tokio::join!(first_handle.get(), second_handle.get());

        assert!(Arc::ptr_eq(&first.unwrap(), &second.unwrap()));
    }

    #[tokio::test]
    async fn tournament_participants_yield_handles_per_id() {
        let storage = SledStorage::builder().build().unwrap();
        let tournament = HydratedGgTournament {
            id: 100,
            name: "Genesis".to_string(),
            participant_ids: vec![1, 2, 3],
        };
        seed(&storage, "tournament", 100, &tournament).await;
        let session = session_over(storage);

        let handles = session.tournament(100).participants().await.unwrap();

        let ids: Vec<StartGgId> = handles.iter().map(|h| h.id()).collect();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn set_players_yield_handles_from_matchup_slots() {
        let storage = SledStorage::builder().build().unwrap();
        let set = HydratedGgSet {
            id: 500,
            completed_at: None,
            round: Some(1),
            matchup: Some(Matchup::Singles {
                left: SlotData {
                    entrant_id: 10,
                    player_id: 11,
                    score: Some(3.0),
                },
                right: SlotData {
                    entrant_id: 20,
                    player_id: 21,
                    score: Some(2.0),
                },
            }),
            games: vec![],
        };
        seed(&storage, "set", 500, &set).await;
        let session = session_over(storage);

        let handles = session.set(500).players().await.unwrap();

        let ids: Vec<StartGgId> = handles.iter().map(|h| h.id()).collect();
        assert_eq!(ids, vec![11, 21]);
    }

    #[tokio::test]
    async fn set_without_matchup_yields_no_players() {
        let storage = SledStorage::builder().build().unwrap();
        let set = HydratedGgSet {
            id: 501,
            completed_at: None,
            round: None,
            matchup: None,
            games: vec![],
        };
        seed(&storage, "set", 501, &set).await;
        let session = session_over(storage);

        let handles = session.set(501).players().await.unwrap();

        assert!(handles.is_empty());
    }
}

use std::{future::Future, num::NonZeroU32, sync::Arc, time::SystemTime};

use bracket_tools_cache::{
    null_storage::NullStorage,
    storage::{Storage, StorageError},
};
use bracket_tools_startgg_schema::{
    get_games_for_set::{GetGamesOfSet, GetGamesOfSetVariables},
    get_player_for_player_id::{GetPlayerForPlayerId, GetPlayerForPlayerIdVariables},
    get_tournament_for_id::{GetTournamentForId, GetTournamentForIdVariables},
};
use cynic::{
    http::{CynicReqwestError, ReqwestExt},
    GraphQlError, GraphQlResponse, Operation, QueryBuilder,
};
use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use reqwest::{
    header::{HeaderMap, HeaderValue, AUTHORIZATION},
    Client,
};
use serde::{de::DeserializeOwned, Serialize};
use thiserror::Error;

use crate::{
    conversions::{extract_tournament_participants_page, tournament_name, GgConversionError, Page, PlayerQueryResult, SetQueryResult},
    gg_data_types::{HydratedGgPlayer, HydratedGgSet, HydratedGgTournament, StartGgId},
    types::GGRestToken,
};

const STARTGG_API_URL: &str = "https://api.start.gg/gql/alpha";
const DEFAULT_REQUESTS_PER_MINUTE: u32 = 80;
const DEFAULT_PAGE_SIZE: i32 = 25;

fn gg_id(id: StartGgId) -> cynic::Id {
    cynic::Id::new(id.to_string())
}

#[derive(Debug, Error)]
pub enum GGProviderError {
    #[error("HTTP error: {0}")]
    Http(#[from] CynicReqwestError),

    #[error("GraphQL errors: {}", format_graphql_errors(.0))]
    GraphQl(Vec<GraphQlError>),

    #[error("conversion error: {0}")]
    Conversion(#[from] GgConversionError),

    #[error("start.gg returned an empty response")]
    EmptyResponse,

    #[error("cache error: {0}")]
    Storage(#[from] StorageError),

    #[error("cache deserialization error: {0}")]
    CacheDeserialization(String),
}

fn format_graphql_errors(errors: &[GraphQlError]) -> String {
    errors.iter().map(|e| e.message.as_str()).collect::<Vec<_>>().join("; ")
}

/// Async client for the start.gg GraphQL API with built-in rate limiting and
/// pluggable storage.
///
/// Use [`NullStorage`] for an uncached provider, or [`SledStorage`](bracket_tools_cache::sled_storage::SledStorage)
/// for persistent caching.
pub struct GGProvider<S: Storage> {
    client: Client,
    rate_limiter: Arc<DefaultDirectRateLimiter>,
    page_size: i32,
    storage: S,
}

impl GGProvider<NullStorage> {
    /// Creates a builder with no caching (uses [`NullStorage`]).
    pub fn builder(token: GGRestToken) -> GGProviderBuilder<NullStorage> {
        GGProviderBuilder::new(token, NullStorage)
    }
}

impl<S: Storage> GGProvider<S> {
    /// Creates a builder with the given storage backend.
    pub fn builder_with_storage(token: GGRestToken, storage: S) -> GGProviderBuilder<S> {
        GGProviderBuilder::new(token, storage)
    }

    /// Waits for rate limit clearance, then executes a cynic GraphQL operation.
    async fn run_query<ResponseData, Vars>(&self, operation: Operation<ResponseData, Vars>) -> Result<ResponseData, GGProviderError>
    where
        Vars: Serialize,
        ResponseData: DeserializeOwned + 'static,
    {
        self.rate_limiter.until_ready().await;

        let response: GraphQlResponse<ResponseData> = self.client.post(STARTGG_API_URL).run_graphql(operation).await?;

        if let Some(errors) = response.errors {
            if !errors.is_empty() {
                return Err(GGProviderError::GraphQl(errors));
            }
        }

        response.data.ok_or(GGProviderError::EmptyResponse)
    }

    /// Fetches every page of a paginated query, accumulating items in page order.
    ///
    /// `build_page` builds the cynic operation for a 1-based page number;
    /// `extract_page` pulls that page's items and total page count from the
    /// response. Returns the accumulated items plus the first page's raw
    /// response, so callers can read any page-1 scalar fields (e.g. a
    /// tournament's name) without issuing a second request.
    async fn fetch_all_pages<T, ResponseData, Vars, B, E>(
        &self,
        build_page: B,
        extract_page: E,
    ) -> Result<(Vec<T>, ResponseData), GGProviderError>
    where
        Vars: Serialize,
        ResponseData: DeserializeOwned + 'static,
        B: Fn(i32) -> Operation<ResponseData, Vars>,
        E: Fn(&ResponseData) -> Result<Page<T>, GgConversionError>,
    {
        let first = self.run_query(build_page(1)).await?;
        let Page { mut items, total_pages } = extract_page(&first)?;

        for page_num in 2..=total_pages {
            let response = self.run_query(build_page(page_num)).await?;
            items.extend(extract_page(&response)?.items);
        }

        Ok((items, first))
    }

    /// Checks the cache for a stored value, falling back to `fetch` on miss.
    /// On a successful fetch, the result is serialized and stored before returning.
    async fn cached_fetch<T, F, Fut>(&self, entity: &str, id: StartGgId, fetch: F) -> Result<T, GGProviderError>
    where
        T: Serialize + DeserializeOwned,
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, GGProviderError>>,
    {
        let key = cache_key(entity, id);

        if let Some((_timestamp, bytes)) = self.storage.get(&key).await? {
            let value: T = bincode::deserialize(&bytes).map_err(|e| GGProviderError::CacheDeserialization(e.to_string()))?;
            return Ok(value);
        }

        let value = fetch().await?;

        let bytes = bincode::serialize(&value).map_err(|e| GGProviderError::CacheDeserialization(e.to_string()))?;
        self.storage.put(&key, SystemTime::now(), &bytes).await?;

        Ok(value)
    }

    /// Fetches a tournament by its start.gg numeric ID.
    ///
    /// Returns a cached result if available, otherwise queries the API and
    /// stores the response.
    pub async fn get_tournament(&self, id: StartGgId) -> Result<HydratedGgTournament, GGProviderError> {
        self.cached_fetch("tournament", id, || self.fetch_tournament(id)).await
    }

    /// Fetches a player by their start.gg numeric ID.
    ///
    /// Returns a cached result if available, otherwise queries the API and
    /// stores the response.
    pub async fn get_player(&self, id: StartGgId) -> Result<HydratedGgPlayer, GGProviderError> {
        self.cached_fetch("player", id, || self.fetch_player(id)).await
    }

    /// Fetches a set and its games by the set's start.gg numeric ID.
    ///
    /// Returns a cached result if available, otherwise queries the API and
    /// stores the response.
    pub async fn get_set_games(&self, id: StartGgId) -> Result<HydratedGgSet, GGProviderError> {
        self.cached_fetch("set", id, || self.fetch_set_games(id)).await
    }

    async fn fetch_tournament(&self, id: StartGgId) -> Result<HydratedGgTournament, GGProviderError> {
        let gg_id = gg_id(id);
        let page_size = self.page_size;

        let (participant_ids, first_page) = self
            .fetch_all_pages(
                |page_num| {
                    GetTournamentForId::build(GetTournamentForIdVariables {
                        t_id: &gg_id,
                        num_per_page: page_size,
                        page_num,
                    })
                },
                extract_tournament_participants_page,
            )
            .await?;

        let name = tournament_name(&first_page)?;

        Ok(HydratedGgTournament { id, name, participant_ids })
    }

    async fn fetch_player(&self, id: StartGgId) -> Result<HydratedGgPlayer, GGProviderError> {
        let gg_id = gg_id(id);
        let operation = GetPlayerForPlayerId::build(GetPlayerForPlayerIdVariables { p_id: &gg_id });
        let data = self.run_query(operation).await?;

        HydratedGgPlayer::try_from(PlayerQueryResult { id, response: data }).map_err(GGProviderError::from)
    }

    async fn fetch_set_games(&self, id: StartGgId) -> Result<HydratedGgSet, GGProviderError> {
        let gg_id = gg_id(id);
        let operation = GetGamesOfSet::build(GetGamesOfSetVariables { s_id: &gg_id });
        let data = self.run_query(operation).await?;

        HydratedGgSet::try_from(SetQueryResult { id, response: data }).map_err(GGProviderError::from)
    }
}

/// Constructs a cache key for a given entity type and start.gg ID.
pub fn cache_key(entity: &str, id: StartGgId) -> String {
    format!("{entity}:{id}")
}

/// Builder for configuring and constructing a [`GGProvider`].
pub struct GGProviderBuilder<S: Storage> {
    token: GGRestToken,
    requests_per_minute: u32,
    page_size: i32,
    storage: S,
}

impl<S: Storage> GGProviderBuilder<S> {
    fn new(token: GGRestToken, storage: S) -> Self {
        Self {
            token,
            requests_per_minute: DEFAULT_REQUESTS_PER_MINUTE,
            page_size: DEFAULT_PAGE_SIZE,
            storage,
        }
    }

    pub fn requests_per_minute(mut self, rpm: u32) -> Self {
        self.requests_per_minute = rpm;
        self
    }

    pub fn page_size(mut self, size: i32) -> Self {
        self.page_size = size;
        self
    }

    pub fn build(self) -> Result<GGProvider<S>, reqwest::Error> {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&self.token.as_bearer_value()).expect("bearer token should be valid ASCII"),
        );

        let client = Client::builder().default_headers(headers).build()?;

        let quota =
            Quota::per_minute(NonZeroU32::new(self.requests_per_minute).unwrap_or(NonZeroU32::new(DEFAULT_REQUESTS_PER_MINUTE).unwrap()));
        let rate_limiter = Arc::new(RateLimiter::direct(quota));

        Ok(GGProvider {
            client,
            rate_limiter,
            page_size: self.page_size,
            storage: self.storage,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{str::FromStr, time::SystemTime};

    use bracket_tools_cache::{null_storage::NullStorage, sled_storage::SledStorage, storage::Storage};

    use super::{GGProvider, DEFAULT_PAGE_SIZE, DEFAULT_REQUESTS_PER_MINUTE};
    use crate::{
        gg_data_types::{HydratedGgPlayer, HydratedGgSet, HydratedGgTournament, Matchup, SlotData},
        types::GGRestToken,
    };

    fn test_token() -> GGRestToken {
        GGRestToken::from_str("91b0c4b4aeae0a040d5b2c0e4d8861c2").unwrap()
    }

    #[test]
    fn builder_defaults() {
        let builder = GGProvider::builder(test_token());
        assert_eq!(builder.requests_per_minute, DEFAULT_REQUESTS_PER_MINUTE);
        assert_eq!(builder.page_size, DEFAULT_PAGE_SIZE);
    }

    #[test]
    fn builder_custom_config() {
        let builder = GGProvider::builder(test_token()).requests_per_minute(40).page_size(50);
        assert_eq!(builder.requests_per_minute, 40);
        assert_eq!(builder.page_size, 50);
    }

    #[test]
    fn builder_produces_provider() {
        let provider = GGProvider::builder(test_token()).build();
        assert!(provider.is_ok());
    }

    #[test]
    fn builder_with_storage() {
        let provider = GGProvider::builder_with_storage(test_token(), NullStorage).build();
        assert!(provider.is_ok());
    }

    #[tokio::test]
    async fn get_player_returns_cached_value() {
        let storage = SledStorage::builder().build().unwrap();
        let expected = HydratedGgPlayer {
            id: 99999,
            gamer_tag: "CachedPlayer".to_string(),
            prefix: Some("TST".to_string()),
        };

        let bytes = bincode::serialize(&expected).unwrap();
        storage.put("player:99999", SystemTime::now(), &bytes).await.unwrap();

        // Provider has a dummy token — if the cache misses, the HTTP request
        // would fail. A successful return proves the hit path worked.
        let provider = GGProvider::builder_with_storage(test_token(), storage).build().unwrap();

        let result = provider.get_player(99999).await.unwrap();
        assert_eq!(result, expected);
    }

    #[tokio::test]
    async fn get_tournament_returns_cached_value() {
        let storage = SledStorage::builder().build().unwrap();
        let expected = HydratedGgTournament {
            id: 88888,
            name: "Cached Tournament".to_string(),
            participant_ids: vec![1, 2, 3],
        };

        let bytes = bincode::serialize(&expected).unwrap();
        storage.put("tournament:88888", SystemTime::now(), &bytes).await.unwrap();

        let provider = GGProvider::builder_with_storage(test_token(), storage).build().unwrap();

        let result = provider.get_tournament(88888).await.unwrap();
        assert_eq!(result, expected);
    }

    #[tokio::test]
    async fn get_set_games_returns_cached_value() {
        let storage = SledStorage::builder().build().unwrap();
        let expected = HydratedGgSet {
            id: 77777,
            completed_at: Some("1700000000".to_string()),
            round: Some(1),
            matchup: Some(Matchup::Singles {
                left: SlotData {
                    entrant_id: 10,
                    player_id: 20,
                    score: Some(3.0),
                },
                right: SlotData {
                    entrant_id: 30,
                    player_id: 40,
                    score: Some(1.0),
                },
            }),
            games: vec![],
        };

        let bytes = bincode::serialize(&expected).unwrap();
        storage.put("set:77777", SystemTime::now(), &bytes).await.unwrap();

        let provider = GGProvider::builder_with_storage(test_token(), storage).build().unwrap();

        let result = provider.get_set_games(77777).await.unwrap();
        assert_eq!(result, expected);
    }
}

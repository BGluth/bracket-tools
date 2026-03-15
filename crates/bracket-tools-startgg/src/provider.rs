use std::num::NonZeroU32;
use std::sync::Arc;

use bracket_tools_cache::null_storage::NullStorage;
use bracket_tools_cache::storage::Storage;
use cynic::http::ReqwestExt;
use cynic::{GraphQlResponse, Operation, QueryBuilder};
use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use thiserror::Error;

use bracket_tools_startgg_schema::{
    get_games_for_set::{GetGamesOfSet, GetGamesOfSetVariables},
    get_player_for_player_id::{GetPlayerForPlayerId, GetPlayerForPlayerIdVariables},
    get_tournament_for_id::{GetTournamentForId, GetTournamentForIdVariables},
};

use crate::{
    conversions::{GgConversionError, PlayerQueryResult, SetQueryResult, TournamentQueryResult},
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
    Http(#[from] cynic::http::CynicReqwestError),

    #[error("GraphQL errors: {}", format_graphql_errors(.0))]
    GraphQl(Vec<cynic::GraphQlError>),

    #[error("conversion error: {0}")]
    Conversion(#[from] GgConversionError),

    #[error("start.gg returned an empty response")]
    EmptyResponse,
}

fn format_graphql_errors(errors: &[cynic::GraphQlError]) -> String {
    errors
        .iter()
        .map(|e| e.message.as_str())
        .collect::<Vec<_>>()
        .join("; ")
}

/// Async client for the start.gg GraphQL API with built-in rate limiting and
/// pluggable storage.
///
/// Use [`NullStorage`] for an uncached provider, or [`SledStorage`](bracket_tools_cache::sled_storage::SledStorage)
/// for persistent caching.
pub struct GGProvider<S: Storage> {
    client: reqwest::Client,
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
    async fn run_query<ResponseData, Vars>(
        &self,
        operation: Operation<ResponseData, Vars>,
    ) -> Result<ResponseData, GGProviderError>
    where
        Vars: serde::Serialize,
        ResponseData: serde::de::DeserializeOwned + 'static,
    {
        self.rate_limiter.until_ready().await;

        let response: GraphQlResponse<ResponseData> = self
            .client
            .post(STARTGG_API_URL)
            .run_graphql(operation)
            .await?;

        if let Some(errors) = response.errors {
            if !errors.is_empty() {
                return Err(GGProviderError::GraphQl(errors));
            }
        }

        response.data.ok_or(GGProviderError::EmptyResponse)
    }

    /// Fetches a tournament by its start.gg numeric ID.
    pub async fn get_tournament(
        &self,
        id: StartGgId,
    ) -> Result<HydratedGgTournament, GGProviderError> {
        let gg_id = gg_id(id);
        let operation = GetTournamentForId::build(GetTournamentForIdVariables {
            t_id: &gg_id,
            num_per_page: self.page_size,
            page_num: 1,
        });
        let data = self.run_query(operation).await?;

        HydratedGgTournament::try_from(TournamentQueryResult { id, response: data })
            .map_err(GGProviderError::from)
    }

    /// Fetches a player by their start.gg numeric ID.
    pub async fn get_player(
        &self,
        id: StartGgId,
    ) -> Result<HydratedGgPlayer, GGProviderError> {
        let gg_id = gg_id(id);
        let operation =
            GetPlayerForPlayerId::build(GetPlayerForPlayerIdVariables { p_id: &gg_id });
        let data = self.run_query(operation).await?;

        HydratedGgPlayer::try_from(PlayerQueryResult { id, response: data })
            .map_err(GGProviderError::from)
    }

    /// Fetches a set and its games by the set's start.gg numeric ID.
    pub async fn get_set_games(
        &self,
        id: StartGgId,
    ) -> Result<HydratedGgSet, GGProviderError> {
        let gg_id = gg_id(id);
        let operation = GetGamesOfSet::build(GetGamesOfSetVariables { s_id: &gg_id });
        let data = self.run_query(operation).await?;

        HydratedGgSet::try_from(SetQueryResult { id, response: data })
            .map_err(GGProviderError::from)
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
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            reqwest::header::HeaderValue::from_str(&self.token.as_bearer_value())
                .expect("bearer token should be valid ASCII"),
        );

        let client = reqwest::Client::builder()
            .default_headers(headers)
            .build()?;

        let quota = Quota::per_minute(
            NonZeroU32::new(self.requests_per_minute)
                .unwrap_or(NonZeroU32::new(DEFAULT_REQUESTS_PER_MINUTE).unwrap()),
        );
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
    use std::str::FromStr;

    use bracket_tools_cache::null_storage::NullStorage;

    use super::{GGProvider, DEFAULT_PAGE_SIZE, DEFAULT_REQUESTS_PER_MINUTE};
    use crate::types::GGRestToken;

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
        let builder = GGProvider::builder(test_token())
            .requests_per_minute(40)
            .page_size(50);
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
}
use std::{
    future::Future,
    num::NonZeroU32,
    sync::Arc,
    time::{Duration, SystemTime},
};

use bracket_tools_cache::{
    null_storage::NullStorage,
    storage::{Storage, StorageError},
};
use bracket_tools_startgg_schema::{
    admin_probe::{AdminProbe, AdminProbeVariables},
    get_event_characters::{GetEventCharacters, GetEventCharactersVariables},
    get_event_structure::{self, GetEventStructure, GetEventStructureVariables},
    get_events_for_tournament::{GetEventsForTournament, GetEventsForTournamentVariables},
    get_games_for_set::{GetGamesOfSet, GetGamesOfSetVariables},
    get_player_for_player_id::{GetPlayerForPlayerId, GetPlayerForPlayerIdVariables},
    get_sets_for_event::{self, GetSetsForEvent, GetSetsForEventVariables},
    get_tournament_for_id::{GetTournamentForId, GetTournamentForIdVariables},
    mark_set_called::{MarkSetCalled, MarkSetCalledVariables},
    mark_set_in_progress::{MarkSetInProgress, MarkSetInProgressVariables},
    report_bracket_set::{BracketSetGameDataInput, BracketSetGameSelectionInput, ReportBracketSet, ReportBracketSetVariables},
};
use cynic::{
    http::{CynicReqwestError, ReqwestExt},
    GraphQlError, GraphQlResponse, MutationBuilder, Operation, QueryBuilder,
};
use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use reqwest::{
    header::{HeaderMap, HeaderValue, AUTHORIZATION},
    Client,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use thiserror::Error;

use crate::{
    conversions::{
        extract_admin_probe, extract_event_characters, extract_event_sets_page, extract_event_structure, extract_mark_set_called,
        extract_mark_set_in_progress, extract_report_bracket_set, extract_tournament_events, extract_tournament_participants_page,
        tournament_name, AdminProbeResult, CharacterInfo, EventInfo, GgConversionError, Page, PlayerQueryResult, SetMutationResult,
        SetQueryResult,
    },
    gg_data_types::{HydratedGgPlayer, HydratedGgSet, HydratedGgTournament, StartGgId},
    types::GGRestToken,
};

/// The start.gg GraphQL endpoint. Public so tools capturing raw responses can
/// hit the same URL the provider does.
pub const STARTGG_API_URL: &str = "https://api.start.gg/gql/alpha";
const DEFAULT_REQUESTS_PER_MINUTE: u32 = 80;
const DEFAULT_PAGE_SIZE: i32 = 25;
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_BURST: u32 = 20;

/// The cached entity kinds. Each is its own cache-key namespace and TTL bucket.
#[derive(Clone, Copy)]
enum CacheEntity {
    Tournament,
    Player,
    Set,
}

impl CacheEntity {
    fn key_prefix(self) -> &'static str {
        match self {
            CacheEntity::Tournament => "tournament",
            CacheEntity::Player => "player",
            CacheEntity::Set => "set",
        }
    }
}

/// Per-entity cache TTLs. `None` means entries of that kind never expire by age.
#[derive(Clone, Copy, Default)]
struct EntityTtls {
    tournament: Option<Duration>,
    player: Option<Duration>,
    set: Option<Duration>,
}

impl EntityTtls {
    fn get(&self, entity: CacheEntity) -> Option<Duration> {
        match entity {
            CacheEntity::Tournament => self.tournament,
            CacheEntity::Player => self.player,
            CacheEntity::Set => self.set,
        }
    }
}

/// Whether a cached value has reached a state start.gg will no longer change,
/// so it can be served from cache indefinitely regardless of TTL.
///
/// "Immutable" is a caching contract, not an absolute guarantee — a completed
/// set can still change via a rare manual correction (DQ, score fix, bracket
/// reset). Those are meant to be surfaced by an explicit purge/recheck rather
/// than by periodically re-fetching data that almost never changes.
trait CacheFreshness {
    fn is_immutable(&self) -> bool;
}

impl CacheFreshness for HydratedGgSet {
    fn is_immutable(&self) -> bool {
        // A completed set is final; an in-progress one still changes.
        self.completed_at.is_some()
    }
}

impl CacheFreshness for HydratedGgPlayer {
    fn is_immutable(&self) -> bool {
        // Players have no terminal state — tag/prefix can change at any time
        // (rarely), so freshness is governed by TTL alone.
        false
    }
}

impl CacheFreshness for HydratedGgTournament {
    fn is_immutable(&self) -> bool {
        // TODO: treat completed tournaments as immutable once `state`/`endAt`
        // are added to the tournament query and `HydratedGgTournament`.
        false
    }
}

/// Whether a value stored at `stored_at` has exceeded its `ttl`.
///
/// A `None` TTL never expires. A timestamp in the future (clock skew) counts
/// as stale, forcing a refresh.
fn is_stale(ttl: Option<Duration>, stored_at: SystemTime) -> bool {
    match ttl {
        None => false,
        Some(ttl) => stored_at.elapsed().map_or(true, |age| age >= ttl),
    }
}

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
///
/// Cache freshness is opt-in per entity via the builder's `*_ttl` methods.
/// Independently, values in a terminal state (e.g. a completed set) are treated
/// as immutable and served from cache regardless of TTL.
pub struct GGProvider<S: Storage> {
    client: Client,
    rate_limiter: Arc<DefaultDirectRateLimiter>,
    page_size: i32,
    ttls: EntityTtls,
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

    /// Returns a cached value when present and still fresh, otherwise fetches,
    /// stores, and returns a new one.
    ///
    /// A cached value is served when it is immutable (terminal state) or within
    /// its entity's TTL; otherwise it is treated as a miss and re-fetched, which
    /// overwrites the stale entry with a fresh timestamp.
    async fn cached_fetch<T, F, Fut>(&self, entity: CacheEntity, id: StartGgId, fetch: F) -> Result<T, GGProviderError>
    where
        T: Serialize + DeserializeOwned + CacheFreshness,
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, GGProviderError>>,
    {
        let key = cache_key(entity.key_prefix(), id);

        if let Some((stored_at, bytes)) = self.storage.get(&key).await? {
            let value: T = bincode::deserialize(&bytes).map_err(|e| GGProviderError::CacheDeserialization(e.to_string()))?;
            if value.is_immutable() || !is_stale(self.ttls.get(entity), stored_at) {
                return Ok(value);
            }
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
        self.cached_fetch(CacheEntity::Tournament, id, || self.fetch_tournament(id)).await
    }

    /// Fetches a player by their start.gg numeric ID.
    ///
    /// Returns a cached result if available, otherwise queries the API and
    /// stores the response.
    pub async fn get_player(&self, id: StartGgId) -> Result<HydratedGgPlayer, GGProviderError> {
        self.cached_fetch(CacheEntity::Player, id, || self.fetch_player(id)).await
    }

    /// Fetches a set and its games by the set's start.gg numeric ID.
    ///
    /// Returns a cached result if available, otherwise queries the API and
    /// stores the response.
    pub async fn get_set_games(&self, id: StartGgId) -> Result<HydratedGgSet, GGProviderError> {
        self.cached_fetch(CacheEntity::Set, id, || self.fetch_set_games(id)).await
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

    /// Fetches every set in an event (all pages), including not-yet-filled
    /// future sets (`hideEmpty: false`), sorted by round.
    ///
    /// Bypasses the cache entirely: pollers want a full fresh snapshot each
    /// time, and stale set data must never be served back.
    pub async fn fetch_event_sets(&self, slug: &str) -> Result<Vec<get_sets_for_event::Set>, GGProviderError> {
        let (sets, _first_page) = self
            .fetch_all_pages(
                |page| {
                    GetSetsForEvent::build(GetSetsForEventVariables {
                        slug,
                        page,
                        per_page: self.page_size,
                    })
                },
                extract_event_sets_page,
            )
            .await?;

        Ok(sets)
    }

    /// Fetches an event's structural skeleton: phases, phase groups (bracket
    /// type, rounds, wave) and entrant count. Bypasses the cache entirely.
    pub async fn fetch_event_structure(&self, slug: &str) -> Result<get_event_structure::Event, GGProviderError> {
        let data = self
            .run_query(GetEventStructure::build(GetEventStructureVariables { slug }))
            .await?;

        Ok(extract_event_structure(data)?)
    }

    /// Probes whether the current token administers a tournament: one query
    /// answering `currentUser` and the admin-only `Tournament.admins` field
    /// (hidden for non-admin tokens — absence is signal, not an error).
    /// Bypasses the cache entirely.
    pub async fn fetch_admin_probe(&self, tournament_id: StartGgId) -> Result<AdminProbeResult, GGProviderError> {
        let gg_id = gg_id(tournament_id);
        let data = self
            .run_query(AdminProbe::build(AdminProbeVariables { tournament_id: &gg_id }))
            .await?;

        Ok(extract_admin_probe(data))
    }

    /// Lists a tournament's events from its tournament slug (either form:
    /// `tournament/foo` or bare `foo`) — the expansion step for tools that
    /// take a whole tournament instead of per-event slugs. Bypasses the cache
    /// entirely; an unknown tournament yields an empty list.
    pub async fn fetch_tournament_events(&self, slug: &str) -> Result<Vec<EventInfo>, GGProviderError> {
        let data = self
            .run_query(GetEventsForTournament::build(GetEventsForTournamentVariables { slug }))
            .await?;

        Ok(extract_tournament_events(data))
    }

    /// Fetches an event's videogame character roster (the vocabulary for
    /// reporting character selections). Bypasses the cache entirely; an event
    /// without character data yields an empty roster.
    pub async fn fetch_event_characters(&self, slug: &str) -> Result<Vec<CharacterInfo>, GGProviderError> {
        let data = self
            .run_query(GetEventCharacters::build(GetEventCharactersVariables { slug }))
            .await?;

        Ok(extract_event_characters(data))
    }

    /// Reports a set's result: the winning entrant, optional per-game winners
    /// and character selections, and the DQ flag. Entrant ids travel as
    /// strings (GraphQL `ID` coercion accepts either form).
    pub async fn report_bracket_set(
        &self,
        id: StartGgId,
        winner_entrant_id: Option<&str>,
        is_dq: bool,
        games: &[GameReport],
    ) -> Result<SetMutationResult, GGProviderError> {
        let gg_id = gg_id(id);
        let game_data = (!games.is_empty()).then(|| games.iter().enumerate().map(|(ix, game)| game_data_input(ix, game)).collect());
        let operation = ReportBracketSet::build(ReportBracketSetVariables {
            set_id: &gg_id,
            winner_id: winner_entrant_id.map(cynic::Id::new),
            is_dq: is_dq.then_some(true),
            game_data,
        });

        self.run_set_mutation(id, operation, move |data| extract_report_bracket_set(data, id))
            .await
    }

    /// Marks a set as called (players summoned to their station).
    pub async fn mark_set_called(&self, id: StartGgId) -> Result<SetMutationResult, GGProviderError> {
        let gg_id = gg_id(id);
        let operation = MarkSetCalled::build(MarkSetCalledVariables { set_id: &gg_id });

        self.run_set_mutation(id, operation, extract_mark_set_called).await
    }

    /// Marks a set as in progress.
    pub async fn mark_set_in_progress(&self, id: StartGgId) -> Result<SetMutationResult, GGProviderError> {
        let gg_id = gg_id(id);
        let operation = MarkSetInProgress::build(MarkSetInProgressVariables { set_id: &gg_id });

        self.run_set_mutation(id, operation, extract_mark_set_in_progress).await
    }

    /// Runs a set-mutating operation, then deletes the set's cache entry.
    ///
    /// The mutation's 4-field payload can't rebuild a cached `HydratedGgSet`,
    /// so delete-invalidation is what restores read-your-writes for cached
    /// providers (a no-op under `NullStorage`).
    async fn run_set_mutation<ResponseData, Vars>(
        &self,
        id: StartGgId,
        operation: Operation<ResponseData, Vars>,
        extract: impl FnOnce(ResponseData) -> Result<SetMutationResult, GgConversionError>,
    ) -> Result<SetMutationResult, GGProviderError>
    where
        Vars: Serialize,
        ResponseData: DeserializeOwned + 'static,
    {
        let data = self.run_query(operation).await?;

        self.storage.delete(&cache_key(CacheEntity::Set.key_prefix(), id)).await?;

        Ok(extract(data)?)
    }
}

/// One game of a set report, in play order (game numbers are assigned from
/// position). Entrant ids are strings so preview-era and synthetic ids pass
/// through unchanged. Serde derives exist for callers that persist queued
/// reports across a restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameReport {
    pub winner_entrant_id: Option<String>,
    pub selections: Vec<GameSelection>,
}

/// One entrant's character pick for one game.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameSelection {
    pub entrant_id: String,
    pub character_id: Option<i32>,
}

fn game_data_input(ix: usize, game: &GameReport) -> BracketSetGameDataInput {
    let selections = (!game.selections.is_empty()).then(|| {
        game.selections
            .iter()
            .map(|s| BracketSetGameSelectionInput {
                entrant_id: cynic::Id::new(&s.entrant_id),
                character_id: s.character_id,
            })
            .collect()
    });
    BracketSetGameDataInput {
        winner_id: game.winner_entrant_id.as_deref().map(cynic::Id::new),
        game_num: ix as i32 + 1,
        entrant1_score: None,
        entrant2_score: None,
        stage_id: None,
        selections,
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
    burst: u32,
    connect_timeout: Duration,
    request_timeout: Duration,
    page_size: i32,
    ttls: EntityTtls,
    storage: S,
}

impl<S: Storage> GGProviderBuilder<S> {
    fn new(token: GGRestToken, storage: S) -> Self {
        Self {
            token,
            requests_per_minute: DEFAULT_REQUESTS_PER_MINUTE,
            burst: DEFAULT_BURST,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            page_size: DEFAULT_PAGE_SIZE,
            ttls: EntityTtls::default(),
            storage,
        }
    }

    pub fn requests_per_minute(mut self, rpm: u32) -> Self {
        self.requests_per_minute = rpm;
        self
    }

    /// Sets how many requests may fire back-to-back before the limiter starts
    /// spacing them out. Clamped to the per-minute rate. Defaults to 20
    /// (previously the implicit burst equaled the full per-minute quota).
    pub fn burst(mut self, burst: u32) -> Self {
        self.burst = burst;
        self
    }

    /// Sets the TCP connect timeout. Defaults to 5s.
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Sets the total per-request timeout (connect + transfer). Defaults to
    /// 15s; without one, a black-holed connection hangs forever.
    pub fn request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    pub fn page_size(mut self, size: i32) -> Self {
        self.page_size = size;
        self
    }

    /// Sets how long cached tournaments stay fresh; tournaments older than this
    /// are re-fetched. Defaults to no expiry.
    pub fn tournament_ttl(mut self, ttl: Duration) -> Self {
        self.ttls.tournament = Some(ttl);
        self
    }

    /// Sets how long cached players stay fresh; players older than this are
    /// re-fetched. Defaults to no expiry.
    pub fn player_ttl(mut self, ttl: Duration) -> Self {
        self.ttls.player = Some(ttl);
        self
    }

    /// Sets how long cached in-progress sets stay fresh; non-completed sets
    /// older than this are re-fetched. Completed sets are immutable and ignore
    /// this. Defaults to no expiry.
    pub fn set_ttl(mut self, ttl: Duration) -> Self {
        self.ttls.set = Some(ttl);
        self
    }

    pub fn build(self) -> Result<GGProvider<S>, reqwest::Error> {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&self.token.as_bearer_value()).expect("bearer token should be valid ASCII"),
        );

        let client = Client::builder()
            .default_headers(headers)
            .connect_timeout(self.connect_timeout)
            .timeout(self.request_timeout)
            .build()?;

        let rpm = NonZeroU32::new(self.requests_per_minute).unwrap_or(NonZeroU32::new(DEFAULT_REQUESTS_PER_MINUTE).unwrap());
        let burst = NonZeroU32::new(self.burst.min(rpm.get())).unwrap_or(NonZeroU32::new(1).unwrap());
        let quota = Quota::per_minute(rpm).allow_burst(burst);
        let rate_limiter = Arc::new(RateLimiter::direct(quota));

        Ok(GGProvider {
            client,
            rate_limiter,
            page_size: self.page_size,
            ttls: self.ttls,
            storage: self.storage,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{
        str::FromStr,
        time::{Duration, SystemTime},
    };

    use bracket_tools_cache::{null_storage::NullStorage, sled_storage::SledStorage, storage::Storage};
    use serde::Serialize;

    use super::{
        is_stale, CacheEntity, CacheFreshness, EntityTtls, GGProvider, DEFAULT_BURST, DEFAULT_CONNECT_TIMEOUT, DEFAULT_PAGE_SIZE,
        DEFAULT_REQUESTS_PER_MINUTE, DEFAULT_REQUEST_TIMEOUT,
    };
    use crate::{
        gg_data_types::{HydratedGgPlayer, HydratedGgSet, HydratedGgTournament, Matchup, SlotData, StartGgId},
        types::GGRestToken,
    };

    fn test_token() -> GGRestToken {
        GGRestToken::from_str("91b0c4b4aeae0a040d5b2c0e4d8861c2").unwrap()
    }

    fn player(id: StartGgId, gamer_tag: &str, prefix: Option<&str>) -> HydratedGgPlayer {
        HydratedGgPlayer {
            id,
            gamer_tag: gamer_tag.to_string(),
            prefix: prefix.map(str::to_string),
        }
    }

    fn completed_set() -> HydratedGgSet {
        HydratedGgSet {
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
        }
    }

    async fn seed_at(storage: &SledStorage, key: &str, stored_at: SystemTime, value: &impl Serialize) {
        let bytes = bincode::serialize(value).unwrap();
        storage.put(key, stored_at, &bytes).await.unwrap();
    }

    #[test]
    fn builder_defaults() {
        let builder = GGProvider::builder(test_token());
        assert_eq!(builder.requests_per_minute, DEFAULT_REQUESTS_PER_MINUTE);
        assert_eq!(builder.page_size, DEFAULT_PAGE_SIZE);
        assert_eq!(builder.burst, DEFAULT_BURST);
        assert_eq!(builder.connect_timeout, DEFAULT_CONNECT_TIMEOUT);
        assert_eq!(builder.request_timeout, DEFAULT_REQUEST_TIMEOUT);
    }

    #[test]
    fn builder_custom_config() {
        let builder = GGProvider::builder(test_token())
            .requests_per_minute(40)
            .page_size(50)
            .burst(5)
            .connect_timeout(Duration::from_secs(2))
            .request_timeout(Duration::from_secs(30));
        assert_eq!(builder.requests_per_minute, 40);
        assert_eq!(builder.page_size, 50);
        assert_eq!(builder.burst, 5);
        assert_eq!(builder.connect_timeout, Duration::from_secs(2));
        assert_eq!(builder.request_timeout, Duration::from_secs(30));
    }

    #[test]
    fn builder_accepts_burst_above_rpm_and_zero_burst() {
        // Clamping happens in build(); both extremes must still produce a provider.
        assert!(GGProvider::builder(test_token()).requests_per_minute(40).burst(500).build().is_ok());
        assert!(GGProvider::builder(test_token()).burst(0).build().is_ok());
    }

    #[test]
    fn builder_sets_per_entity_ttls() {
        let builder = GGProvider::builder(test_token())
            .tournament_ttl(Duration::from_secs(300))
            .player_ttl(Duration::from_secs(86400))
            .set_ttl(Duration::from_secs(60));
        assert_eq!(builder.ttls.tournament, Some(Duration::from_secs(300)));
        assert_eq!(builder.ttls.player, Some(Duration::from_secs(86400)));
        assert_eq!(builder.ttls.set, Some(Duration::from_secs(60)));
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

    #[test]
    fn is_stale_without_ttl_is_never_stale() {
        let a_year_ago = SystemTime::now() - Duration::from_secs(60 * 60 * 24 * 365);
        assert!(!is_stale(None, a_year_ago));
    }

    #[test]
    fn is_stale_within_ttl_is_fresh() {
        let ten_secs_ago = SystemTime::now() - Duration::from_secs(10);
        assert!(!is_stale(Some(Duration::from_secs(60)), ten_secs_ago));
    }

    #[test]
    fn is_stale_past_ttl_is_stale() {
        let two_mins_ago = SystemTime::now() - Duration::from_secs(120);
        assert!(is_stale(Some(Duration::from_secs(60)), two_mins_ago));
    }

    #[test]
    fn is_stale_future_timestamp_is_stale() {
        let an_hour_ahead = SystemTime::now() + Duration::from_secs(3600);
        assert!(is_stale(Some(Duration::from_secs(60)), an_hour_ahead));
    }

    #[test]
    fn entity_ttls_select_per_entity() {
        let ttls = EntityTtls {
            set: Some(Duration::from_secs(30)),
            ..Default::default()
        };
        assert_eq!(ttls.get(CacheEntity::Set), Some(Duration::from_secs(30)));
        assert_eq!(ttls.get(CacheEntity::Player), None);
        assert_eq!(ttls.get(CacheEntity::Tournament), None);
    }

    #[test]
    fn completed_set_is_immutable() {
        assert!(completed_set().is_immutable());
    }

    #[test]
    fn in_progress_set_is_not_immutable() {
        let mut set = completed_set();
        set.completed_at = None;
        assert!(!set.is_immutable());
    }

    #[test]
    fn players_and_tournaments_are_never_immutable() {
        let tournament = HydratedGgTournament {
            id: 1,
            name: "Genesis".to_string(),
            participant_ids: vec![],
        };
        assert!(!player(1, "Mang0", None).is_immutable());
        assert!(!tournament.is_immutable());
    }

    #[tokio::test]
    async fn get_player_returns_cached_value() {
        let storage = SledStorage::builder().build().unwrap();
        let expected = player(99999, "CachedPlayer", Some("TST"));
        seed_at(&storage, "player:99999", SystemTime::now(), &expected).await;

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
        seed_at(&storage, "tournament:88888", SystemTime::now(), &expected).await;

        let provider = GGProvider::builder_with_storage(test_token(), storage).build().unwrap();

        let result = provider.get_tournament(88888).await.unwrap();
        assert_eq!(result, expected);
    }

    #[tokio::test]
    async fn get_set_games_returns_cached_value() {
        let storage = SledStorage::builder().build().unwrap();
        let expected = completed_set();
        seed_at(&storage, "set:77777", SystemTime::now(), &expected).await;

        let provider = GGProvider::builder_with_storage(test_token(), storage).build().unwrap();

        let result = provider.get_set_games(77777).await.unwrap();
        assert_eq!(result, expected);
    }

    #[tokio::test]
    async fn fresh_player_within_ttl_is_served() {
        let storage = SledStorage::builder().build().unwrap();
        let expected = player(99999, "Fresh", None);
        seed_at(&storage, "player:99999", SystemTime::now(), &expected).await;

        let provider = GGProvider::builder_with_storage(test_token(), storage)
            .player_ttl(Duration::from_secs(3600))
            .build()
            .unwrap();

        let result = provider.get_player(99999).await.unwrap();
        assert_eq!(result, expected);
    }

    #[tokio::test]
    async fn old_player_without_ttl_is_served() {
        let storage = SledStorage::builder().build().unwrap();
        let expected = player(99998, "Ancient", None);
        let an_hour_ago = SystemTime::now() - Duration::from_secs(3600);
        seed_at(&storage, "player:99998", an_hour_ago, &expected).await;

        let provider = GGProvider::builder_with_storage(test_token(), storage).build().unwrap();

        let result = provider.get_player(99998).await.unwrap();
        assert_eq!(result, expected);
    }

    #[tokio::test]
    async fn completed_set_served_from_cache_despite_short_ttl() {
        // A completed set is immutable, so it is served even when stored well
        // before its TTL window — terminal data ignores staleness. A miss here
        // would hit the network with a dummy token and fail.
        let storage = SledStorage::builder().build().unwrap();
        let expected = completed_set();
        let an_hour_ago = SystemTime::now() - Duration::from_secs(3600);
        seed_at(&storage, "set:77777", an_hour_ago, &expected).await;

        let provider = GGProvider::builder_with_storage(test_token(), storage)
            .set_ttl(Duration::from_secs(1))
            .build()
            .unwrap();

        let result = provider.get_set_games(77777).await.unwrap();
        assert_eq!(result, expected);
    }
}

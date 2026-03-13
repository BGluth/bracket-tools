use std::time::Duration;

use serde::Deserialize;
use thiserror::Error;

use crate::{
    provider::{Provider, ProviderError},
    query_cache::{CacheKey, CacheableKey, QueryCache, QueryCacheBuilder, QueryCacheError},
};

#[derive(Clone, Debug, Error)]
pub enum CacheableProviderError {
    #[error(transparent)]
    QueryCache(#[from] QueryCacheError),

    #[error(transparent)]
    Provider(#[from] ProviderError),
}

#[derive(Debug)]
pub struct CacheQuery {

}

#[derive(Debug)]
pub struct CachedProviderCfg {
    stale_time: Duration,
    query_cache_builder: QueryCacheBuilder,
}

#[derive(Debug)]
pub struct CachedProviderBuilder {
    cfg: CachedProviderCfg,
}

impl From<CachedProviderCfg> for CachedProviderBuilder {
    fn from(v: CachedProviderCfg) -> Self {
        todo!()
    }
}

impl CachedProviderBuilder {
    pub fn build<P: Provider>(self, p: P) -> CachedProvider<P> {
        todo!()
    }
}

/// A wrapper around a provider that always attempts to check the cache before the provider.
///
/// The provider is generally going to be an upstream, but this is not guaranteed.
#[derive(Clone, Debug)]
pub struct CachedProvider<P> {
    cache: QueryCache,

    remote_p: P,

    /// The amount of time until a record is considered stale.
    stale_time: Duration,
}

impl<P: Provider> From<P> for CachedProvider<P> {
    fn from(v: P) -> Self {
        todo!()
    }
}

impl<P: Provider> Provider for CachedProvider<P> {
    type ProviderQueryPayload = CacheQuery;

    fn get<'de, Q, V>(&self, q: Q) -> V
    where
        Q: Into<Self::ProviderQueryPayload>,
        V: Deserialize<'de>,
    {
        todo!()
    }
}

impl<P: Provider> CachedProvider<P> {
    pub fn from_provider(p: P, cfg: CachedProviderCfg) -> Self {
        CachedProviderBuilder::from(cfg).build(p)
    }
}

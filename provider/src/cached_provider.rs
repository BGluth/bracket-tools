use std::time::Duration;

use serde::Deserialize;
use thiserror::Error;

use crate::{
    provider::{Provider, ProviderError},
    query_cache::{CacheKey, QueryCache, QueryCacheBuilder, QueryCacheError},
};

pub trait CacheableProvider: Provider {
    /// This is pretty unstable and may change.
    ///
    /// The idea is all providers may store whatever data they want. However, we want to somewhat unify the way it's stored. For now, we're
    /// going to say that a piece of data can be retrieved by the data type and the id.
    fn get_cache_key(&self, k: &Self::Key) -> CacheKey;
}

type CacheableProviderResult<T> = Result<T, CacheableProviderError>;

#[derive(Clone, Debug, Error)]
pub enum CacheableProviderError {
    #[error(transparent)]
    QueryCache(#[from] QueryCacheError),

    #[error(transparent)]
    Provider(#[from] ProviderError),
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
    type Key = P::Key;

    fn get<'de, V: Deserialize<'de>>(&self, k: Self::Key) -> V {
        todo!()
    }
}

impl<P: Provider> CachedProvider<P> {
    pub fn from_provider(p: P, cfg: CachedProviderCfg) -> Self {
        CachedProviderBuilder::from(cfg).build(p)
    }
}

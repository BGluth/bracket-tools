use std::time::Duration;

use serde::Deserialize;
use thiserror::Error;

use crate::{
    provider::{Provider, ProviderError},
    query_cache::{CacheKey, QueryCacheError},
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

/// A wrapper around a provider that always attempts to check the cache before the provider.
///
/// The provider is generally going to be an upstream, but this is not guaranteed.
#[derive(Clone, Debug)]
pub struct CachedProvider<P> {
    remote_p: P,

    /// The amount of time until a record is considered stale.
    stale_time: Duration,
}

impl<P: CacheableProvider + Provider> CachedProvider<P> {
    /// Attempts to get/fetch a value for a key.
    ///
    /// Always checks the cache first, and otherwise fetches from the provider. Returns `None` if the values does not exist upstream.
    fn get<'a, V: Deserialize<'a>>(&'a self, k: P::Key) -> CacheableProviderResult<Option<V>> {
        todo!()
    }
}

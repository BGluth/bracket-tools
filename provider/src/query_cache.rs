use bytesize::ByteSize;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const DEFAULT_MEM_CACHE_SIZE: ByteSize = ByteSize::gb(2);
const DEFAULT_DISK_CACHE_SIZE: ByteSize = ByteSize::gb(50);

pub(crate) type QueryCacheResult<T> = Result<T, QueryCacheError>;

#[derive(Clone, Debug, Error)]
pub enum QueryCacheError {}

#[derive(Debug)]
pub struct QueryCacheBuilder {
    max_mem_cache_size: ByteSize,
    max_disk_cache_size: ByteSize,
}

impl From<QueryCacheBuilder> for QueryCache {
    fn from(v: QueryCacheBuilder) -> Self {
        v.build()
    }
}

impl Default for QueryCacheBuilder {
    fn default() -> Self {
        Self {
            max_disk_cache_size: DEFAULT_MEM_CACHE_SIZE,
            max_mem_cache_size: DEFAULT_DISK_CACHE_SIZE,
        }
    }
}

impl QueryCacheBuilder {
    pub(crate) fn max_mem_cache_size(mut self, max_mem_cache_size: ByteSize) -> Self {
        self.max_mem_cache_size = max_mem_cache_size;
        self
    }

    pub(crate) fn max_disk_cache_size(mut self, max_disk_cache_size: ByteSize) -> Self {
        self.max_disk_cache_size = max_disk_cache_size;
        self
    }

    pub(crate) fn build(self) -> QueryCache {
        todo!()
    }
}

/// A generic cache that captures queries for a given provider.
///
/// Cache is also serialized to disk.
#[derive(Debug)]
pub(crate) struct QueryCache {}

impl QueryCache {
    pub(crate) fn get<'de, K, V>(&self, k: K) -> QueryCacheResult<V>
    where
        K: AsRef<[u8]>,
        V: Deserialize<'de>,
    {
        todo!()
    }

    pub(crate) fn update<K, V>(&mut self, k: K, v: V) -> QueryCacheResult<()>
    where
        K: AsRef<[u8]>,
        V: Serialize,
    {
        todo!()
    }

    pub(crate) fn clear(&mut self) -> QueryCacheResult<()> {
        todo!()
    }
}

// TODO: Remove the use of `String`s...
#[derive(Clone, Debug)]
pub struct CacheKey {
    d_type: String,
    id: String,
}

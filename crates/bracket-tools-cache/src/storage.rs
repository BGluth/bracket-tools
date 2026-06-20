use std::{future::Future, time::SystemTime};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("storage I/O error: {0}")]
    Io(String),

    #[error("deserialization error: {0}")]
    Deserialization(String),
}

/// Async key-value storage for cached API responses.
///
/// Keys are string identifiers (e.g. `"tournament:12345"`).
/// Values are opaque byte slices paired with a timestamp recording when
/// the entry was stored. Staleness and TTL decisions live above this trait.
pub trait Storage: Send + Sync {
    fn get(&self, key: &str) -> impl Future<Output = Result<Option<(SystemTime, Vec<u8>)>, StorageError>>;

    fn put(&self, key: &str, timestamp: SystemTime, value: &[u8]) -> impl Future<Output = Result<(), StorageError>>;

    fn delete(&self, key: &str) -> impl Future<Output = Result<(), StorageError>>;

    fn clear(&mut self) -> impl Future<Output = Result<(), StorageError>>;
}

use std::time::SystemTime;

use crate::storage::{Storage, StorageError};

/// A no-op storage backend that never caches anything.
///
/// `get` always returns `None`; all other operations succeed immediately.
/// Use this when you want an uncached `GGProvider`.
#[derive(Clone, Debug, Default)]
pub struct NullStorage;

impl Storage for NullStorage {
    async fn get(&self, _key: &str) -> Result<Option<(SystemTime, Vec<u8>)>, StorageError> {
        Ok(None)
    }

    async fn put(&self, _key: &str, _timestamp: SystemTime, _value: &[u8]) -> Result<(), StorageError> {
        Ok(())
    }

    async fn delete(&self, _key: &str) -> Result<(), StorageError> {
        Ok(())
    }

    async fn clear(&mut self) -> Result<(), StorageError> {
        Ok(())
    }
}
use std::{
    collections::HashMap,
    sync::{Mutex, MutexGuard},
    time::SystemTime,
};

use crate::storage::{Storage, StorageError};

/// Stored entries keyed by their string identifier, paired with the timestamp
/// recording when each was written.
type Entries = HashMap<String, (SystemTime, Vec<u8>)>;

/// In-process storage backend backed by a `HashMap`.
///
/// Entries live only for the lifetime of the process — nothing is persisted to
/// disk. This makes it well suited to tests and to the reporter TUI, where a
/// fast cache is wanted without the overhead or side effects of an on-disk
/// database.
///
/// The map is wrapped in a `Mutex` for interior mutability; every critical
/// section is a single `HashMap` operation, so the lock is held only briefly.
#[derive(Debug, Default)]
pub struct MemoryStorage {
    entries: Mutex<Entries>,
}

impl MemoryStorage {
    /// Create an empty `MemoryStorage`.
    pub fn new() -> Self {
        Self::default()
    }

    fn lock_entries(&self) -> Result<MutexGuard<'_, Entries>, StorageError> {
        self.entries.lock().map_err(|e| StorageError::Io(e.to_string()))
    }
}

impl Storage for MemoryStorage {
    async fn get(&self, key: &str) -> Result<Option<(SystemTime, Vec<u8>)>, StorageError> {
        Ok(self.lock_entries()?.get(key).cloned())
    }

    async fn put(&self, key: &str, timestamp: SystemTime, value: &[u8]) -> Result<(), StorageError> {
        self.lock_entries()?.insert(key.to_string(), (timestamp, value.to_vec()));
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<(), StorageError> {
        self.lock_entries()?.remove(key);
        Ok(())
    }

    async fn clear(&mut self) -> Result<(), StorageError> {
        self.lock_entries()?.clear();
        Ok(())
    }
}

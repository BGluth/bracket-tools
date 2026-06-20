use std::{fmt::Display, sync::Mutex, time::SystemTime};

use serde::{Deserialize, Serialize};
use sled::Db;

use crate::storage::{Storage, StorageError};

/// Timestamped value wrapper for sled serialization.
#[derive(Serialize, Deserialize)]
struct StoredEntry {
    timestamp: SystemTime,
    data: Vec<u8>,
}

/// Persistent storage backend using an embedded sled database.
///
/// The inner `Db` is wrapped in a `Mutex` because sled 1.0-alpha's `Db` is
/// `!Sync` (thread-local caches use `RefCell`). All operations are synchronous
/// under the hood, so the lock is held only briefly.
#[derive(Debug)]
pub struct SledStorage {
    db: Mutex<Db>,
}

impl SledStorage {
    pub fn builder() -> SledStorageBuilder {
        SledStorageBuilder { path: None }
    }
}

/// Builder for configuring and constructing a [`SledStorage`].
pub struct SledStorageBuilder {
    path: Option<String>,
}

impl SledStorageBuilder {
    /// Set the filesystem path for the sled database.
    /// If not set, sled will use a temporary directory.
    pub fn path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    pub fn build(self) -> Result<SledStorage, StorageError> {
        let config = match self.path {
            Some(path) => sled::Config::new().path(path),
            None => sled::Config::tmp().map_err(to_io_err)?,
        };

        let db = config.open().map_err(to_io_err)?;

        Ok(SledStorage { db: Mutex::new(db) })
    }
}

fn to_io_err(e: impl Display) -> StorageError {
    StorageError::Io(e.to_string())
}

fn lock_db(db: &Mutex<Db>) -> Result<std::sync::MutexGuard<'_, Db>, StorageError> {
    db.lock().map_err(|e| StorageError::Io(e.to_string()))
}

impl Storage for SledStorage {
    async fn get(&self, key: &str) -> Result<Option<(SystemTime, Vec<u8>)>, StorageError> {
        let db = lock_db(&self.db)?;

        let Some(bytes) = db.get(key.as_bytes()).map_err(to_io_err)? else {
            return Ok(None);
        };

        let entry: StoredEntry = bincode::deserialize(&bytes).map_err(|e| StorageError::Deserialization(e.to_string()))?;

        Ok(Some((entry.timestamp, entry.data)))
    }

    async fn put(&self, key: &str, timestamp: SystemTime, value: &[u8]) -> Result<(), StorageError> {
        let entry = StoredEntry {
            timestamp,
            data: value.to_vec(),
        };
        let serialized = bincode::serialize(&entry).map_err(to_io_err)?;

        let db = lock_db(&self.db)?;
        db.insert(key.as_bytes(), serialized).map_err(to_io_err)?;

        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<(), StorageError> {
        let db = lock_db(&self.db)?;
        db.remove(key.as_bytes()).map_err(to_io_err)?;
        Ok(())
    }

    async fn clear(&mut self) -> Result<(), StorageError> {
        let db = lock_db(&self.db)?;
        db.clear().map_err(to_io_err)?;
        Ok(())
    }
}

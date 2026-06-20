use std::time::SystemTime;

use bracket_tools_cache::{memory_storage::MemoryStorage, null_storage::NullStorage, sled_storage::SledStorage, storage::Storage};

// NullStorage has its own semantics (it never persists), so it doesn't share
// the behavioral helpers below.

#[tokio::test]
async fn null_storage_get_returns_none() {
    let storage = NullStorage;
    let result = storage.get("tournament:12345").await.unwrap();
    assert!(result.is_none());
}

#[tokio::test]
async fn null_storage_put_succeeds() {
    let storage = NullStorage;
    let now = SystemTime::now();
    storage.put("key", now, b"data").await.unwrap();
    // Still returns None — NullStorage never caches
    assert!(storage.get("key").await.unwrap().is_none());
}

// Behavioral helpers shared by every persisting backend. Each backend gets thin
// `#[tokio::test]` wrappers below so a failure still names which backend broke.

async fn check_roundtrip<S: Storage>(storage: S) {
    let now = SystemTime::now();
    let data = b"hello world";

    storage.put("test:1", now, data).await.unwrap();

    let (timestamp, retrieved) = storage.get("test:1").await.unwrap().expect("should exist");
    assert_eq!(timestamp, now);
    assert_eq!(retrieved, data);
}

async fn check_get_missing_key<S: Storage>(storage: S) {
    let result = storage.get("nonexistent").await.unwrap();
    assert!(result.is_none());
}

async fn check_delete<S: Storage>(storage: S) {
    let now = SystemTime::now();

    storage.put("key", now, b"data").await.unwrap();
    assert!(storage.get("key").await.unwrap().is_some());

    storage.delete("key").await.unwrap();
    assert!(storage.get("key").await.unwrap().is_none());
}

async fn check_clear<S: Storage>(mut storage: S) {
    let now = SystemTime::now();

    storage.put("a", now, b"1").await.unwrap();
    storage.put("b", now, b"2").await.unwrap();

    storage.clear().await.unwrap();

    assert!(storage.get("a").await.unwrap().is_none());
    assert!(storage.get("b").await.unwrap().is_none());
}

async fn check_overwrite<S: Storage>(storage: S) {
    let t1 = SystemTime::now();

    storage.put("key", t1, b"first").await.unwrap();
    storage.put("key", t1, b"second").await.unwrap();

    let (_, data) = storage.get("key").await.unwrap().expect("should exist");
    assert_eq!(data, b"second");
}

#[tokio::test]
async fn sled_storage_roundtrip() {
    check_roundtrip(SledStorage::builder().build().unwrap()).await;
}

#[tokio::test]
async fn sled_storage_get_missing_key() {
    check_get_missing_key(SledStorage::builder().build().unwrap()).await;
}

#[tokio::test]
async fn sled_storage_delete() {
    check_delete(SledStorage::builder().build().unwrap()).await;
}

#[tokio::test]
async fn sled_storage_clear() {
    check_clear(SledStorage::builder().build().unwrap()).await;
}

#[tokio::test]
async fn sled_storage_overwrite() {
    check_overwrite(SledStorage::builder().build().unwrap()).await;
}

#[tokio::test]
async fn memory_storage_roundtrip() {
    check_roundtrip(MemoryStorage::new()).await;
}

#[tokio::test]
async fn memory_storage_get_missing_key() {
    check_get_missing_key(MemoryStorage::new()).await;
}

#[tokio::test]
async fn memory_storage_delete() {
    check_delete(MemoryStorage::new()).await;
}

#[tokio::test]
async fn memory_storage_clear() {
    check_clear(MemoryStorage::new()).await;
}

#[tokio::test]
async fn memory_storage_overwrite() {
    check_overwrite(MemoryStorage::new()).await;
}

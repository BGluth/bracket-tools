use std::time::SystemTime;

use bracket_tools_cache::null_storage::NullStorage;
use bracket_tools_cache::sled_storage::SledStorage;
use bracket_tools_cache::storage::Storage;

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

#[tokio::test]
async fn sled_storage_roundtrip() {
    let storage = SledStorage::builder().build().unwrap();
    let now = SystemTime::now();
    let data = b"hello world";

    storage.put("test:1", now, data).await.unwrap();

    let (timestamp, retrieved) = storage.get("test:1").await.unwrap().expect("should exist");
    assert_eq!(timestamp, now);
    assert_eq!(retrieved, data);
}

#[tokio::test]
async fn sled_storage_get_missing_key() {
    let storage = SledStorage::builder().build().unwrap();
    let result = storage.get("nonexistent").await.unwrap();
    assert!(result.is_none());
}

#[tokio::test]
async fn sled_storage_delete() {
    let storage = SledStorage::builder().build().unwrap();
    let now = SystemTime::now();

    storage.put("key", now, b"data").await.unwrap();
    assert!(storage.get("key").await.unwrap().is_some());

    storage.delete("key").await.unwrap();
    assert!(storage.get("key").await.unwrap().is_none());
}

#[tokio::test]
async fn sled_storage_clear() {
    let mut storage = SledStorage::builder().build().unwrap();
    let now = SystemTime::now();

    storage.put("a", now, b"1").await.unwrap();
    storage.put("b", now, b"2").await.unwrap();

    storage.clear().await.unwrap();

    assert!(storage.get("a").await.unwrap().is_none());
    assert!(storage.get("b").await.unwrap().is_none());
}

#[tokio::test]
async fn sled_storage_overwrite() {
    let storage = SledStorage::builder().build().unwrap();
    let t1 = SystemTime::now();

    storage.put("key", t1, b"first").await.unwrap();
    storage.put("key", t1, b"second").await.unwrap();

    let (_, data) = storage.get("key").await.unwrap().expect("should exist");
    assert_eq!(data, b"second");
}
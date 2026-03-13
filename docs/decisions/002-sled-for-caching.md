# ADR 002: sled for Disk-backed Caching

## Status

Accepted.

## Context

The SDK needs a persistent disk cache to avoid redundant API calls to start.gg.
The cache must be embeddable (no external process), performant for key-value
lookups, and straightforward to integrate into async Rust code.

### Alternatives Considered

- **SQLite** (via rusqlite or sqlx): Mature and well-tested but adds a C
  dependency and requires more ceremony for simple key-value storage.
- **RocksDB** (via rust-rocksdb): Battle-tested at scale but brings a large C++
  dependency and complex build requirements.

## Decision

Use sled as the disk-backed cache. sled is pure Rust, has a clean API for
key-value operations, and avoids native dependency complications. It is still in
alpha, which carries some risk of breaking changes or bugs, but this is
acceptable for a caching layer where data loss is recoverable (the cache can
always be rebuilt from the upstream API).

The cache implementation is wrapped behind a thin trait (`Provider`) so the
storage backend can be swapped in the future without changing consumer code.

## Consequences

- No native/C dependencies for the caching layer.
- Pure Rust build with no external toolchain requirements.
- Alpha-quality library risk is contained: cache corruption means a cold start,
  not data loss.
- The `Provider` trait provides an escape hatch if sled needs to be replaced
  later.

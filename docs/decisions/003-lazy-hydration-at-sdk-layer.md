# ADR 003: Move Lazy Hydration from Core Types to SDK Layer

## Status

Accepted. Implementation began in Session 2.

## Context

Early versions of bracket-tools-core embedded lazy-loading directly into the
normalized data types using `Rc<UnsafeCell<...>>`. This approach had two
problems:

1. **Soundness**: `Rc<UnsafeCell<T>>` is not `Send` or `Sync`, making the types
   unusable in multi-threaded async contexts without unsafe workarounds.
2. **Coupling**: Core types carried runtime behavior (network fetching) that
   conceptually belongs in the SDK layer, not in data definitions.

## Decision

Refactor core types into plain data structs with no interior mutability or
lazy-loading logic. All lazy hydration moves to the SDK layer
(bracket-tools-startgg), which uses `tokio::sync::OnceCell` to lazily fetch and
cache related data on demand.

This keeps core types simple, `Send + Sync`, and usable across threads without
unsafe code.

## Consequences

- Core types are plain structs that derive standard traits (`Clone`, `Debug`,
  `Serialize`, `Deserialize`) without complications.
- Core types are safe to share across threads and async tasks.
- Lazy loading is an SDK concern, not a data modeling concern.
- Consumer code that wants eager loading can call SDK methods upfront; code that
  wants lazy loading uses the SDK's `OnceCell`-backed accessors.
- The `Rc<UnsafeCell<...>>` pattern is fully removed.

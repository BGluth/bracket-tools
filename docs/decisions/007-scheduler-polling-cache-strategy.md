# ADR 007: Scheduler Polling and Cache Strategy

## Status

Proposed (stub — completed as the Scheduler V1 build proceeds).

## Context

The scheduler polls all of a tournament's events every ~30 seconds during a
live bracket. The SDK's existing cache (`cached_fetch` + Storage backends) is
built for entity-by-id reads with TTL/immutability semantics — the wrong shape
for a poller that must never act on stale bracket state.

## Decision (proposed)

- **Uncached full-snapshot polling.** Each poll fetches every event's complete
  set list (`hideEmpty: false`) and swaps it in whole, per event. No deltas,
  no cache reads. The scheduler runs on `GGProvider<NullStorage>`.
- **Read-side cache bypass.** The provider's event-level methods
  (`fetch_event_sets`, `fetch_event_structure`) are uncached network
  primitives by design, following the `fetch_*` vs `get_*` naming convention.
  A cached `get_event_*` variant (owned event model + `CacheEntity::Event` +
  event-completed immutability + request coalescing) is future work for
  scraper-style consumers.
- **Write-side delete-invalidation.** The `markSet*` mutations return a
  4-field payload that cannot rebuild a cached `HydratedGgSet`, so after a
  successful mutation the provider deletes the set's cache entry
  (`storage.delete(cache_key("set", id))`) instead of writing through. A no-op
  under NullStorage; restores read-your-writes under persistent backends.

## Consequences

- The scheduler's view is at most one poll interval stale, by construction.
- To be completed during the build week: observed request budgets at burst 20,
  poll-interval tuning, per-event swap/debounce behavior, and whether any
  scheduler state deserves persistence beyond the JSON crash-recovery overlay.
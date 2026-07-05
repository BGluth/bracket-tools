# bracket-tools

Rust mono-repo for esports tournament tooling, primarily targeting the start.gg platform. Built by Brendan, a community leader / TO / player for Alberta Smash (primarily Ultimate). The public-facing goal is an SDK for querying start.gg with built-in caching and rate limiting. Internal tools include a set-reporter TUI, a background scraper daemon, a scheduler, and regional rankings. Development cadence is roughly one day per week.

## Crate Map

| Crate | Directory | Status | Purpose |
|---|---|---|---|
| bracket-tools-core | crates/bracket-tools-core | ~60% | Normalized data types and traits |
| bracket-tools-cache | crates/bracket-tools-cache | ~70% | Async Storage trait + NullStorage + SledStorage + MemoryStorage |
| bracket-tools-query | crates/bracket-tools-query | ~5% | Abstract query interface (multi-platform) |
| bracket-tools-startgg-schema | crates/bracket-tools-startgg-schema | ~85% | cynic codegen types from start.gg schema |
| bracket-tools-startgg | crates/bracket-tools-startgg | ~82% | Main SDK: caching, rate-limited start.gg client |
| reporter-cli | tools/reporter/reporter-cli | ~20% | ratatui TUI for set reporting |
| reporter-state | tools/reporter/reporter-state | ~25% | Reporter state management (store pattern) |
| bracket-tools-scheduler | tools/scheduler | ~15% | Multi-bracket calling tool for the TO desk |
| bracket-tools-daemon | tools/daemon | ~5% | Background scraper daemon |
| edmonton-smash | web/edmonton-smash | Early | Leptos community website (not in workspace yet) |

## Current Phase

Phase 1 -- Foundation (paused for the Scheduler V1 build week). Sessions 1-11 complete. Completed: Entrant.id fix + Matchup enum, fixture tests, GGRestToken refactor, cache layer integration (Storage trait + GGProvider<S>), cache-hit path, lazy-hydration session layer (GgSession + smart handles), pagination (generic `fetch_all_pages`), MemoryStorage (HashMap-backed Storage backend), cache freshness (per-entity TTL + terminal-state immutability). Remaining: request coalescing, doubles.

Scheduler V1 (tools/scheduler, ADR 006/007): S1 complete (session 14) -- SDK event surface (`get_sets_for_event` with hideEmpty:false, `get_event_structure`, first mutations `markSetCalled`/`markSetInProgress` with delete-invalidation, shared i64 Timestamp + tolerant Id scalars, HTTP timeout/burst knobs), `SetSource` trait + tokio Send spike, smoke bin -> live capture gave a GO verdict (empty future sets carry prereq fields; unstarted brackets use preview_* set ids that become numeric at bracket start). Next: S2 -- pure core (bracket graph, ConflictIndex, greedy policy, simulator).

**Housekeeping:** At the end of each session, update the crate status percentages in the table above, run `/handoff` to refresh the active task file in `memory/tasks/` (and `/epic update` for the Phase 1 epic), and refresh the codebase map (`.claude/rules/codebase_map.md`) — only update sections that changed.

## Session Workflow

- **Session start:** Read the `MEMORY.md` Active Work section, then the linked Phase 1 epic (`memory/epics/phase_1_foundation.md`) and active task file(s) in `memory/tasks/`. Summarize what was completed last session, list the candidate tasks from the epic backlog, and ask the user what they'd like to work on.
- **Task completion:** After committing and pushing, ask the user if they want to pick up another task or wrap the session. **This project does not use pull requests** — do not create or offer PRs; commit and push the branch directly.

## Key Technical Decisions

Detailed rationale lives in `docs/decisions/`. Summary:

- **Sled for caching** (002) -- embedded, zero-config, good enough for local tooling.
- **Lazy hydration at SDK layer, not core** (003) -- core types stay plain; the SDK handles cache-miss fetches.
- **Reqwest over surf** (004) -- reqwest is wired into GGProvider; surf fully removed.
- **Multi-platform query abstraction maintained** (005) -- query layer is not start.gg-specific.
- **Nightly Rust** is acceptable for this project.

## Conventions

See `.claude/rules/rust_files.md` for Rust style conventions (naming, error handling, module layout, etc.).

## Architecture

See `docs/architecture.md` for the dependency graph and data-flow diagram.

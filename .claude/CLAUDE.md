# bracket-tools

Rust mono-repo for esports tournament tooling, primarily targeting the start.gg platform. Built by Brendan, a community leader / TO / player for Alberta Smash (primarily Ultimate). The public-facing goal is an SDK for querying start.gg with built-in caching and rate limiting. Internal tools include a set-reporter TUI, a background scraper daemon, a scheduler, and regional rankings. Development cadence is roughly one day per week.

## Crate Map

| Crate | Directory | Status | Purpose |
|---|---|---|---|
| bracket-tools-core | crates/bracket-tools-core | ~40% | Normalized data types and traits |
| bracket-tools-cache | crates/bracket-tools-cache | ~30% | Generic sled-based caching + Provider trait |
| bracket-tools-query | crates/bracket-tools-query | ~5% | Abstract query interface (multi-platform) |
| bracket-tools-startgg-schema | crates/bracket-tools-startgg-schema | ~80% | cynic codegen types from start.gg schema |
| bracket-tools-startgg | crates/bracket-tools-startgg | ~25% | Main SDK: caching, rate-limited start.gg client |
| reporter-cli | tools/reporter/reporter-cli | ~20% | ratatui TUI for set reporting |
| reporter-state | tools/reporter/reporter-state | ~25% | Reporter state management (store pattern) |
| bracket-tools-daemon | tools/daemon | ~5% | Background scraper daemon |
| edmonton-smash | web/edmonton-smash | Early | Leptos community website (not in workspace yet) |

## Current Phase

Phase 1 -- Foundation. Session 1 (workspace bootstrap) is complete. Next up is Session 2: simplify core types, verify schema codegen.

## Key Technical Decisions

Detailed rationale lives in `docs/decisions/`. Summary:

- **Sled for caching** (002) -- embedded, zero-config, good enough for local tooling.
- **Lazy hydration at SDK layer, not core** (003) -- core types stay plain; the SDK handles cache-miss fetches.
- **Reqwest over surf** (004) -- migration pending; surf is the current HTTP backend.
- **Multi-platform query abstraction maintained** (005) -- query layer is not start.gg-specific.
- **Nightly Rust** is acceptable for this project.

## Conventions

See `.claude/rules/rust_files.md` for Rust style conventions (naming, error handling, module layout, etc.).

## Architecture

See `docs/architecture.md` for the dependency graph and data-flow diagram.

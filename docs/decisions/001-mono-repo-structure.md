# ADR 001: Mono-repo Structure

## Status

Accepted.

## Context

All crates were previously nested under a `scraper_gg/` subdirectory, which
made workspace management awkward and complicated relative path references
between crates. As the number of crates grew, the nested layout became
increasingly difficult to navigate.

## Decision

Adopt a flat workspace layout with top-level directories organized by purpose:

- `crates/` -- library crates (core, cache, query, startgg, startgg-schema)
- `tools/` -- binary crates (reporter-cli, reporter-state, daemon)
- `web/` -- web-related crates (if any)

A single `Cargo.toml` at the repository root defines the workspace and lists all
members. Each crate maintains its own `Cargo.toml` with explicit dependencies on
sibling crates via `path` references.

## Consequences

- Simpler `cargo` commands: `cargo build`, `cargo test`, and `cargo clippy` run
  across the entire workspace by default.
- Shared dependency versions are managed in the workspace `Cargo.toml` via
  `[workspace.dependencies]`.
- CI can operate on the whole workspace in a single pass.
- Adding a new crate requires creating a directory and adding it to the
  workspace members list.

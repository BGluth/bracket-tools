# ADR 004: Replace surf with reqwest for HTTP

## Status

Accepted. Migration began in Session 4. surf is kept temporarily during the
transition.

## Context

The project originally used surf as its HTTP client. surf is now unmaintained,
receives no security updates, and has an uncertain future. Continuing to depend
on it introduces supply-chain risk and limits access to newer HTTP features.

## Decision

Migrate to reqwest as the HTTP client. reqwest is actively maintained, widely
used in the Rust ecosystem, and supported by cynic via the `http-reqwest`
feature flag. The migration also simplifies the dependency tree by aligning with
the client that most other crates in the ecosystem already use.

During the transition, surf remains as a dependency in crates that have not yet
been migrated. It will be fully removed once all call sites are ported.

## Consequences

- Active maintenance and security updates for the HTTP layer.
- cynic's `http-reqwest` feature provides direct integration with the GraphQL
  query builder.
- Temporary dual-dependency on surf and reqwest until migration is complete.
- No API changes visible to consumers of the SDK; the HTTP client is an internal
  implementation detail.

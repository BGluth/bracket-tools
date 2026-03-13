# ADR 005: Maintain Multi-platform Query Abstraction

## Status

Accepted.

## Context

bracket-tools-query defines traits that abstract over tournament data sources.
Currently, only start.gg is implemented. The abstraction adds indirection that
has no immediate payoff, and there is a reasonable argument for removing it until
a second platform is actually needed (YAGNI).

However, the esports ecosystem has multiple tournament platforms (Challonge,
Tonamel, various regional platforms), and supporting additional sources is a
stated project goal. Removing the abstraction now would require reintroducing it
later, touching every consumer in the process.

## Decision

Keep the multi-platform query abstraction in bracket-tools-query. The traits
define a small, stable interface (query tournaments, brackets, sets, players)
that maps naturally to what any tournament platform provides. The cost of
maintaining the abstraction is low relative to the cost of retrofitting it later.

## Consequences

- Consumer code written against the query traits will work with future platform
  implementations without modification.
- The trait surface area must remain general enough to accommodate platforms
  beyond start.gg.
- There is a small ongoing maintenance cost for an abstraction with only one
  concrete implementation today.
- Adding a new platform (e.g., Challonge) requires implementing the query traits
  for that platform's API, with no changes to consumer code.

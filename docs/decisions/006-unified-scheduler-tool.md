# ADR 006: Unified Scheduler Tool as a New Crate

## Status

Accepted (Session 14, S1 of the Scheduler V1 build week).

## Context

Scheduler V1 is a multi-bracket calling tool for a single TO desk: it polls
every event in a tournament, maintains the bracket DAG, and recommends which
sets to call onto which setups (writing `markSetCalled`/`markSetInProgress`
back to start.gg). The repo already contains reporter-cli, a ratatui TUI for
per-set reporting, which raises the question of whether the scheduler should
extend it or live elsewhere. The tool must run live at a real tournament on a
tight deadline, on a single Linux machine at the TO desk.

## Decision

Build the scheduler as a new `tools/scheduler` crate (`bracket-tools-scheduler`)
rather than extending reporter-cli.

- The two tools have different shapes: the reporter is per-set data entry; the
  scheduler is a whole-venue polling/optimization loop with its own state
  model, write queue, and crash-recovery overlay.
- reporter-cli stays untouched and remains a candidate for later absorption
  into the scheduler as a reporting pane once the scheduler's Elm-loop TUI is
  proven.
- The scheduler is generic over a `SetSource` trait. The trait is not a test
  double: its second implementation is the `--simulate` fixture-replay source
  used for dress rehearsal and as the read-only fallback mode.
- Windows support is dropped from V1 (the venue machine is Linux; terminal
  lifecycle handling is simpler for it).

## Consequences

- A new workspace member (`tools/scheduler`) with path deps on
  bracket-tools-startgg, -cache, and -startgg-schema.
- In S1 the scheduler's SDK-facing methods return schema-layer types; the
  scheduler-local set model arrives with the poller (S3), keeping the spike
  small.
- reporter-cli and the scheduler may temporarily duplicate small TUI
  utilities; deduplication is deferred until the absorption decision.

# Scheduler — Ops Runbook (DRAFT, S4)

One-page desk procedure for running the multi-bracket calling tool live. **Draft** — S5
finalizes and prints it, and it is walked through cold by a co-TO at the mid-week dress
rehearsal. This document survives every software cut: if a feature below was cut, its
paper fallback is what you follow.

The single most important guarantee: **the desk board — not the start.gg site — is
authoritative for sets the desk manages.** Everything here protects that.

## Before doors (Thursday night + Friday morning)

1. **Token.** The API token lives at `~/work/tokens/scraper_gg.token` (or point `token_file`
   in the config, or pass `-t`, or set `STARTGG_TOKEN`). Never commit it. Never run two
   things on the same token during the event (see lockfile + rate budget below).
2. **Preflight, standalone.** Run `scheduler --config <file> --preflight-only`. It:
   - proves the token authenticates and (S4) probes whether it is a **tournament admin** —
     if not, the tool launches **ADVISOR-ONLY** (no writes) and says so loudly;
   - asserts every configured event belongs to ONE tournament (a mismatch is almost always
     a slug typo);
   - fetches each event's structure + full set list (doubles as the first snapshot);
   - reports "published but not started/seeded — checks deferred" for unstarted brackets
     (this is the EXPECTED Thursday state, not an error);
   - scans for player-identity splits (same tag, different start.gg ids across events) and
     suggests `[[player_aliases]]` entries.
   Fix anything it flags in the config, then re-run until clean.
3. **Capture path.** Preflight prints the active capture directory. Confirm it is writable.
   Capture is ON by default (`--no-capture` disables); it is the post-event debugging trail.
4. **Web-UI CALLED-int capture (advisor-only insurance).** If the token is NOT an admin,
   remote-call detection depends on a pinned `known_called_state_int` (live value: `6`).
   Confirm it is set in the config; otherwise the tool over-filters conservatively (soft-busy)
   and warns.

## Launching for the event

`scheduler --config <file>` (add `--advisor-only` to force read-only regardless of admin
rights — the safe default for a review/rehearsal). The tool opens on the setup board.

- If the network is down at launch, it opens on the last-good persisted snapshot with a
  stale banner rather than a blank screen.
- A second launch on the same state file hard-errors "already running (pid N)" — the flock
  lockfile prevents two instances from halving the rate budget and corrupting state.

## The calling loop (hot path)

- **Digit (setup number)** → opens the call-picker for that setup, top-ranked set preselected.
  **Enter** commits the call locally (and enqueues `markSetCalled` if writes are armed);
  **Esc** cancels. Enter re-checks the set is still callable before committing.
- **p** — players seated → set goes In-Progress (enqueues `markSetInProgress`).
- **f** — set finished at the desk → frees the setup immediately (players free at once; the
  result is confirmed remotely within a poll or the targeted force-poll).
- **r** — no-show → re-queues the set locally. The site may still show it CALLED; that
  mismatch is tracked in the divergence ledger (see handover).
- **z** snooze · **u** undo (single level) · **q** / Ctrl-C quit.
- **a** (S4) — reassign a setup to another bracket / allow-any / restore config pool, when a
  bracket finishes early and its setups should redeploy. **If `a` was cut:** edit the
  bracket `pool` lists in the config and restart (the restart preserves your overlay and
  re-runs preflight safely — see below).
- **i** inspect (why a set is not callable) · **n** notices/log · **w** pending writes +
  divergence ledger · **?** context help.

Calls happen by **voice**; the start.gg writes are bookkeeping and never block advising. If
writes are parked (non-admin, or a mutation failed), keep calling — the queue keeps working.

## Restart to reconfigure (the config-edit fallback for everything)

Config changes take effect on **restart only**. A restart is safe mid-event:

- your local overlay (board, flags, tombstones, snoozes, pending writes, durations, unread
  notices) is reloaded from the state file;
- each event re-runs preflight with three-bucket classification — a transiently-unreachable
  event launches stale-flagged and keeps retrying (NO demote prompt); only a definitive
  failure (bad slug, failed structure assertion) prompts a per-event conflict_only downgrade;
- no-show countdowns and wait-times resume from the wall clock;
- you'll see reconciliation notices for anything dropped (unknown setup ids, stale pool
  overrides). These are informational.

Use this to add setups, change pools, pin a state int, or tune durations without losing state.

## conflict_only discipline (the ANY-bracket guarantee)

A `mode = "conflict_only"` bracket is polled and its players count as busy everywhere, but it
is never called or ranked by the tool. When you voice-call a set in such a bracket, the tool
has no remote evidence mid-set — so either mark it called on the **site**, or use the local
mark key so the conflict filter knows those players are busy. Skipping this can let the tool
suggest a set for a player who is already on stream in the RR/conflict_only bracket.

## Connectivity loss (venue wifi)

Per-event staleness ages climb with inline flags; the global "STALE — verify with desk"
header fires only on total connectivity loss or all events past threshold. Local calling
continues from the last-good tables; writes accumulate visibly in the pending queue and
retry on reconnect (each intent revalidated against a fresh poll for its event before it
flushes). **Mitigation: tether the laptop to a phone hotspot.**

## Desk handover / co-TO

- `?` lists the keys by context; the inspection view (`i`) answers "where did that set go"
  without radio calls.
- The **divergence ledger** (`w`) lists every set the desk re-queued that the site still
  shows CALLED — hand this to the incoming TO as the reconciliation script so they don't
  chase phantom calls on the admin bracket.
- If the laptop dies, fall back to the start.gg **web UI** (there is no co-TO machine — V1 is
  Linux-laptop-only). The site is shared truth; the tool is an advisor on top of it.

## Crash recovery

A panic restores the terminal, flushes state, and writes the message + backtrace to the log
and stderr. Relaunch: the tool reloads the overlay + last-good snapshot and reconciles the
moment polling recovers. If the "STATE NOT PERSISTING" badge is up, the state file is not
writable — fix the path and restart to re-enable crash safety.

## Do NOT during the event

- Run the smoke/capture bin on the desk token (halves the rate budget; the lockfile also
  refuses it while the tool holds the lock). Use a separate token if you must.

## Escalation

Contact: _<fill in before print>_.

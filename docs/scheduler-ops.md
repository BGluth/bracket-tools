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

- No `--config`? The tool looks for `./scheduler.toml`, then the XDG config dir
  (`~/.config/bracket-tools/scheduler.toml`). If neither exists, a live launch writes a
  fully-commented starter there and exits for you to fill in — it never contacts start.gg
  on an unreviewed config. (Offline modes instead derive a ready-to-run config, below.)
- Default state/snapshot files live in the XDG data dir
  (`~/.local/share/bracket-tools/`), so launching from any directory finds the same state;
  `state_file`/`snapshot_file` in the config still override.
- If the network is down at launch, it opens on the last-good persisted snapshot with a
  stale banner rather than a blank screen.
- A second launch on the same state file hard-errors "already running (pid N)" — the flock
  lockfile prevents two instances from halving the rate budget and corrupting state.

## The calling loop (hot path)

- **Digit (setup number)** → opens the call-picker for that setup, top-ranked set preselected.
  The title says which ranking you're looking at: **rollout** (ranked by projected
  tournament finish, freshly simulated — the normal case seconds after a setup frees) or
  **greedy (rollout pending)** (the instant structural ranking). A cyan **HOLD** row means
  the simulation thinks leaving the setup open beats every call — Enter on it just closes
  the picker. **Enter** on a set commits the call locally (and enqueues `markSetCalled` if
  writes are armed); **Esc** cancels. Enter re-checks the set is still callable first.
- **p** — players seated → set goes In-Progress (enqueues `markSetInProgress`).
- **f** — set finished at the desk → frees the setup immediately (players free at once; the
  result is confirmed remotely within a poll or the targeted force-poll).
- **r** — no-show → re-queues the set locally. The site may still show it CALLED; that
  mismatch is tracked in the divergence ledger (see handover).
- **g** — report the selected setup's set (writes-armed only). Tap **1**/**2** per game for
  the winner (a known best-of auto-finishes on the clinch), **Backspace** un-records,
  **Enter** → summary → **y** submits via `reportBracketSet` and frees the setup like `f`.
  Optional characters: **c** opens a prefix-search picker (left player then right, **Tab**
  keeps the current pick); picks apply to every game of the set and stick per player, so
  regulars only need picking once. **d** inside the modal is the confirm-first DQ: pick the
  DQ'd side, review the summary, **y** submits winner-only with the DQ flag. **Esc** steps
  back a stage (games ← characters/DQ/confirm) before it cancels.
- **d** — player flags for the highlighted queue entry: Enter cycles
  resting → departed → force-available → clear. Departed players' sets leave the queue and
  project at zero.
- **z** snooze · **u** undo (single level) · **q** / Ctrl-C quit.
- **a** — reassign the selected setup: dedicate it to one bracket / allow any / restore the
  config pools, for when a bracket finishes early and its setups should redeploy. Free
  setups whose brackets all finished show **N:done→a** in the strip. (Editing the config
  `pool` lists and restarting remains a safe fallback — the restart preserves your overlay
  and re-runs preflight.)
- **i** inspect (why a set is not callable) · **n** notices/log · **w** pending writes +
  divergence ledger · **?** context help.

Calls happen by **voice**; the start.gg writes are bookkeeping and never block advising. If
writes are parked (non-admin, or a mutation failed), keep calling — the queue keeps working.

## Dress rehearsal (`--simulate` + `--pace`)

Drill the whole calling loop against the capture corpus, no network, no live tournament:

```
scheduler --config examples/fbr-100.toml \
          --simulate ~/work/personal/bracket-tools-captures/2026-07-05_s1_smoke \
          --pace 8
```

- `--pace FACTOR` scripts the captured world forward (a full simulated run using the
  config's own setups and duration priors) and plays it back at FACTOR× real time. The FBR
  corpus is ~6½ hours of tournament: `--pace 8` compresses it into ~50 minutes, `--pace 1`
  replays at live speed. Without `--pace`, `--simulate` serves the captures as a static
  (never-changing) world.
- The launch banner reports the script: bracket count, frame count, playback length, and a
  warning for any bracket the simulation could not play to completion.
- Results arrive **on the script's schedule**, standing in for desk web-UI entry. Follow the
  tool's recommendations and your calls stay in step with the script; deviate and you get
  no-shows/deviation notices — drill those flows too, they're real.
- Preview ids are materialized to numeric up front and "drop-in N" placeholder players fill
  the bye-degenerate slots the server would fill live — both expected, not bugs.
- A simulate run persists to `.sim` sibling state files, so a rehearsal never touches (or
  races) live desk state. Writes go to the fixture recorder, never the network; to drill the
  writes-armed flow, use a config without `advisor_only = true` (the fixture answers as a
  full admin).
- Need a fresh corpus (e.g. this week's unstarted brackets)? The smoke bin captures a whole
  tournament in one flag — every event, no per-event slugs:

  ```
  cargo run -p bracket-tools-scheduler --bin smoke -- \
      --token-file ~/work/tokens/scraper_gg.token \
      --tournament tournament/<tourney-slug> --out <captures-dir>
  ```

  (`--event` still works for cherry-picking, and combines with `--tournament`.) Unstarted
  published brackets capture as full preview skeletons — exactly what `--pace` rehearses.

## Autoplay replay (`--autoplay` / `--replay`) and synthetic worlds (`--synth`)

Want to *see* the scheduler's decisions without driving the TUI? Let the sim play the whole
tournament itself and read (or watch) the tape:

```
scheduler --simulate <captures-dir> --autoplay          # real corpus
scheduler --synth de:32,de:16,swiss:8 --autoplay        # parameterized fake brackets
scheduler --synth fbr --autoplay                        # the built-in 7-event FBR-shaped world
scheduler --replay scheduler-replay.txt                 # watch it animated (--frame-ms to pace)
```

- `--autoplay` runs headless: at every free setup the sim commits the greedy ranker's top
  call and the run renders to `scheduler-replay.txt` (`--replay-out` to change) — one frame
  per call/result showing the setup board, per-bracket progress bars, and the call's score
  ingredients ("why: depth 8 · ironman 5 · unblocks 2 · waited 6m"), then a flat decision
  log and a summary with per-bracket finish times and the makespan.
- Every call frame also explains itself against the field: an "over:" line names the
  runner-up candidate and which term decided it (the header states the policy — longest
  critical path first, so the deepest bracket flooding the setups early is by design), and
  a small joined block shows the changed bracket neighborhood: where each player came from
  and where the winner/loser go next.
- The file is plain text (`less` works); `--replay <file>` pages it in the terminal like a
  flipbook — auto-advancing with colour on a tty (`NO_COLOR` respected), space pauses,
  arrow keys step back/forward (PgUp/PgDn ±10, Home/End jump), `q` quits. Replays carry
  real player names when generated from captures — they are gitignored, keep them out of
  the public repo.
- `--noise FRAC` (with `--noise-seed N`, both for `--autoplay` and `--pace`) roughs up the
  sim's set durations: each set gets a fixed seed-derived multiplier in `1 ± FRAC`, so
  rounds stagger organically instead of finishing in lockstep. Off by default; the same
  seed replays the identical run. Config equivalent: `[sim] duration_noise` / `noise_seed`.
- `--synth SPEC` builds a fake tournament from parameters instead of captures: comma-
  separated `kind:entrants` entries (`de`, `se`, `rr`, `swiss` — swiss takes an optional
  `:rounds`), or the literal `fbr`. Adjacent events share ~half their players so
  cross-bracket conflicts are real. Works with everything `--simulate` does: zero-config,
  `--pace`, `--autoplay`, or just poking at the TUI on a world that costs nothing.
- All offline modes derive a ready-to-run config when none exists (largest captured
  tournament, 8 shared setups, writes fixture-armed, state pinned to `.sim` files). A
  *discovered* config (`./scheduler.toml` or the XDG path) naming no event in the offline
  world — the live starter template, say — is ignored with a notice in favor of a derived
  one; an explicit `--config` is always honored.

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

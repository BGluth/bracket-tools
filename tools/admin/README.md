# gg-admin — tournament-admin desk tool (prototype)

Registration admin for the TO desk, built on the public start.gg API.

```sh
# Roster view (works with any token)
gg-admin roster french-bread-rumble-100
gg-admin roster french-bread-rumble-100 --unpaid          # server-side unpaid filter
gg-admin roster french-bread-rumble-100 --event singles   # one event's registrants

# Add a registered player to more events (e.g. a redemption bracket).
# Fuzzy-resolves the tag against the tournament's own participants,
# shows the plan, asks for confirmation, then re-fetches to verify.
gg-admin add french-bread-rumble-100 mango --event redemption

# Fuzzy player search across the saved pool of tournaments, ranked by
# match quality -> recency -> attendance. Prints user ids ready for
# `add --user-id`.
gg-admin find mango                          # searches the saved pool
gg-admin find mango --tournament fbr-42     # pool + extras (remembered)

# The pool (~/.config/bracket-tools/find-pool.toml) grows automatically:
# every tournament the tool touches is remembered. Seed it in bulk:
gg-admin pool scan french-bread-rumble-100  # whole series (same owner + slug stem)
gg-admin pool scan fbr-100 --all            # everything by that owner
gg-admin pool scan --mine                   # everything YOUR token administers
gg-admin pool list                          # show it (add/remove also exist)
```

Past tournaments' rosters are cached under the XDG data dir (their entry
lists are frozen), so repeated `find` runs over a big pool stay off the
network; recent/upcoming tournaments always refetch, and `find --refresh`
forces everything live.

Tournament arguments accept a bare slug, `tournament/slug`, or a full
start.gg URL. Token resolution: `--token-file` > `$STARTGG_TOKEN` >
`~/work/tokens/admin_gg.token` > `~/work/tokens/scraper_gg.token`.

## Caveats (public-API limits)

- **Marking a player paid is impossible via the API.** There is no payment
  mutation; `--unpaid` (a query filter) is the only view of payment state.
  Flipping someone to paid stays in the start.gg admin UI.
- **`add` needs a tournament-admin token** and uses the on-behalf-of flow
  (`generateRegistrationToken` → `registerForTournament`). As of the first
  prototype this flow has not been exercised live — test on a throwaway
  event first. Paid events may refuse token registration or register the
  player unpaid.
- **`find` searches only the pool** — the public API has no global player
  search. Scan your series into it and it doubles as a burner detector
  (real locals show repeated, recent attendance).

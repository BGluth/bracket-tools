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

# Fuzzy player search across past tournaments (the local-scene pool),
# ranked by match quality -> recency -> attendance. Prints user ids
# ready for `add --user-id`.
gg-admin find mango --tournament fbr-99 --tournament fbr-100
```

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
- **`find` searches only the tournaments you name** — the public API has no
  global player search. Feed it your recent events and it doubles as a
  burner detector (real locals show repeated, recent attendance).

# gg-admin — tournament-admin desk tool (prototype)

Registration and bracket admin for the TO desk, built on the public start.gg API.

```sh
# Roster view (works with any token)
gg-admin roster french-bread-rumble-100
gg-admin roster french-bread-rumble-100 --unpaid          # server-side unpaid filter
gg-admin roster french-bread-rumble-100 --event singles   # one event's registrants

# Register players into events. Resolves tags against the tournament's own
# participants, or takes user ids straight from `find`; shows the plan, asks
# once, then re-reads the event's entrants to verify. See the caveat below:
# only the token's own account can actually be registered through the API.
gg-admin add rust_vitational --event singles --user-id 1808768
gg-admin add rust_vitational --event singles --user-id 1524176 --user-id 1090798 --dry-run

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

# Bracket structure: phases and their groups.
gg-admin phases rust_vitational
gg-admin phase set rust_vitational --event singles --phase Bracket --bracket double-elimination --groups 1
gg-admin phase delete rust_vitational --event singles --phase "Super Top 8"
gg-admin phase create rust_vitational --event singles --name "Top 8" --bracket double-elimination

# Seeding: list players best-first (tags, entrant names or user ids). Anyone
# left out keeps their relative order behind the listed players.
gg-admin seed rust_vitational --event singles perendeshe "prince hunter" drem --dry-run
gg-admin seed rust_vitational --event singles perendeshe "prince hunter" drem

# Standings, and the redemption fill: everyone who placed 5th or lower in
# singles gets registered into the redemption event (created in the UI first).
gg-admin standings colchester-clash-28 --event singles
gg-admin redemption colchester-clash-28 --from singles --to redemption --place 5 --dry-run
```

Past tournaments' rosters are cached under the XDG data dir (their entry
lists are frozen), so repeated `find` runs over a big pool stay off the
network; recent/upcoming tournaments always refetch, and `find --refresh`
forces everything live.

Tournament arguments accept a bare slug, `tournament/slug`, or a full
start.gg URL. Token resolution: `--token-file` > `$STARTGG_TOKEN` >
`~/work/tokens/admin_gg.token` > `~/work/tokens/scraper_gg.token`.

## Caveats (public-API limits)

- **Only the token's own account can be registered.** The API's add-attendee
  pair is `generateRegistrationToken(userId, events)` →
  `registerForTournament(events, token)`, but the server looks the token up
  under the *calling* user, so redeeming one minted for someone else fails
  with "RegistrationToken not found" (live-verified 2026-10-24, admin token,
  free event). `add` and `redemption` therefore register the token's own
  user and print everyone else as an Add-Attendee list for the start.gg
  admin UI (Attendees → Add attendee). Players can also register themselves
  while registration is open.
- **The participant roster lags.** `Tournament.participants` is served from
  a search index that trails writes by several minutes; `Event.entrants` is
  immediate. Write verification uses entrants for that reason, and a
  `roster` run right after a registration may not show it yet.
- **Events cannot be created.** A redemption bracket is a second event; make
  it in the admin UI, then `redemption` fills it from the main event's
  standings once those exist.
- **Marking a player paid is impossible via the API.** There is no payment
  mutation; `--unpaid` (a query filter) is the only view of payment state.
- **`find` searches only the pool** — the public API has no global player
  search. Scan your series into it and it doubles as a burner detector
  (real locals show repeated, recent attendance).

# gg-watch — tournament watcher and feed publisher

Sweeps start.gg for the tournaments in a region, reports what appeared,
changed, or vanished since the last sweep, and publishes the current window
as JSON and iCalendar feeds. One-shot: run it on a timer.

```sh
gg-watch tick              # sweep, report changes, write feeds, save the snapshot
gg-watch tick --dry-run    # sweep and report only
gg-watch status            # the last snapshot
gg-watch paths             # where the config, snapshot, and feeds live
```

The first tick records the window without reporting anything as new. A sweep
that comes back empty against a populated snapshot is refused rather than
reported as mass removals. A sweep that fails writes nothing.

## Config

`~/.config/bracket-tools/watch.toml` (override with `--config`):

```toml
# Optional; the --token-file flag and $STARTGG_TOKEN take precedence.
token_file = "~/work/tokens/scraper_gg.token"
# How long a finished tournament stays in the sweep and the feeds (default 14).
history_days = 14

# At least one of country / state / radius is required.
[region]
country = "US"
state = "CA"
# radius = { lat = 34.05, lng = -118.24, distance = "50mi" }
# Only tournaments with an event in these games (start.gg ids); empty = all.
videogame_ids = [1, 1386]

# Labels. A tournament belongs to the first series whose owner matches (if
# set) and whose slug stem (`weekly-42` -> `weekly`) is listed (if any are).
[[series]]
name = "The Weekly"
owner_id = 123456
stems = ["weekly", "the-weekly"]

[feeds]
# Defaults: ~/.local/share/bracket-tools/watch/tournaments.{json,ics}
json = "~/sites/events/tournaments.json"
ics = "~/sites/events/tournaments.ics"
calendar_name = "Local tournaments"
```

Use a non-admin token so the sweep sees exactly what the public sees.

## Feeds

- `tournaments.json`: the window in start order, one flat object per
  tournament (id, slug, url, name, series, times, venue, registration, games,
  events).
- `tournaments.ics`: one `VEVENT` per dated tournament with a stable UID
  (`startgg-<id>@gg-watch`), so a calendar subscribed to the file's URL sees
  edits as edits and removals as removals. Host the file somewhere reachable
  and subscribe by URL; calendar apps poll on their own schedule.

## Running on a timer

A systemd user timer; `Persistent=true` runs a missed tick at the next boot.

```ini
# ~/.config/systemd/user/gg-watch.service
[Service]
Type=oneshot
ExecStart=%h/.cargo/bin/gg-watch tick

# ~/.config/systemd/user/gg-watch.timer
[Timer]
OnBootSec=2min
OnUnitActiveSec=10min
Persistent=true
[Install]
WantedBy=timers.target
```

```sh
systemctl --user enable --now gg-watch.timer
journalctl --user -u gg-watch -f
```

Run exactly one copy: the snapshot is local state, and two producers would
each report the same changes.

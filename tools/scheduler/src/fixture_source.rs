//! A [`SetSource`] that replays fixtures instead of hitting start.gg: raw
//! capture envelopes (the S1 smoke corpus), synthetic snapshot sequences
//! built from `synth`, and a hang mode proving the poller's timeout path.
//!
//! Every fetch still round-trips through the schema layer, so the real
//! conversion (`live_sets_from_schema`) is exercised on every replayed poll.
//! Mutations are recorded and answered synthetically; they do NOT edit the
//! scripted snapshots — the scripted timeline is the source of truth for
//! what the "server" reports next.

use std::{
    collections::{HashMap, HashSet},
    fs,
    future::pending,
    path::{Path, PathBuf},
    slice::from_ref,
    sync::Mutex,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use bracket_tools_startgg::{AdminProbeResult, CharacterInfo, GameReport, SetMutationResult, StartGgId};
use bracket_tools_startgg_schema::{
    enums::{ActivityState, BracketType},
    get_event_structure, get_sets_for_event,
    scalars::{Id, Timestamp},
};
use cynic::GraphQlResponse;
use thiserror::Error;

use crate::{
    config::{BracketConfig, SchedulerConfig, SetupCounts},
    model::{GroupKind, LiveSet, PhaseGroupInfo, Prereq, Slot, PREREQ_TYPE_SEED, PREREQ_TYPE_SET},
    set_source::SetSource,
    synth::{make_de_bracket_with, make_rr_pool_with, make_se_bracket_with, make_swiss_with, materialize_ids, tagged_players},
};

/// The state ints the fixture answers mutations with, matching live
/// observations (CALLED=6, IN_PROGRESS=2, COMPLETED=3).
pub const FIXTURE_CALLED_INT: i32 = 6;
pub const FIXTURE_IN_PROGRESS_INT: i32 = 2;
pub const FIXTURE_COMPLETED_INT: i32 = 3;
/// Setup pool size for a derived offline config.
pub const SIM_SETUP_COUNT: u32 = 8;
/// `--synth` worlds get numeric ids from the start (calls exercise the full
/// write path); below the rehearsal's 9_900_000_000 base so a later `--pace`
/// materialization pass keeps them.
const SYNTH_ID_BASE: u64 = 9_000_000_000;
const SYNTH_ID_STRIDE: u64 = 1_000_000;
const SYNTH_PG_BASE: u64 = 2001;
const SYNTH_TOURNAMENT: &str = "tournament/synth";
/// The character roster every fixture event answers with (Smash 64 cast —
/// small, recognizable, and enough to exercise prefix search).
pub const FIXTURE_ROSTER: [&str; 12] = [
    "Mario",
    "Donkey Kong",
    "Link",
    "Samus",
    "Yoshi",
    "Kirby",
    "Fox",
    "Pikachu",
    "Luigi",
    "Ness",
    "Captain Falcon",
    "Jigglypuff",
];

#[derive(Debug, Error)]
pub enum FixtureError {
    #[error("fixture has no event {0:?}")]
    UnknownEvent(String),
}

#[derive(Debug, Error)]
pub enum FixtureLoadError {
    #[error("failed to read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: Box<serde_json::Error>,
    },

    #[error("{path}: envelope carried no data")]
    EmptyEnvelope { path: PathBuf },

    #[error("no capture events under {dir}")]
    NoEvents { dir: PathBuf },
}

#[derive(Debug, Error)]
pub enum SynthSpecError {
    #[error("empty --synth spec")]
    Empty,

    #[error("bad --synth entry {entry:?}: {reason} (expected kind:entrants with kind de|se|rr|swiss, e.g. de:32)")]
    Bad { entry: String, reason: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationKind {
    Called,
    InProgress,
}

/// One write the fixture answered, in arrival order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MutationRecord {
    pub kind: MutationKind,
    pub id: StartGgId,
}

/// One set report the fixture answered, in arrival order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportRecord {
    pub set_id: StartGgId,
    pub winner_entrant_id: Option<String>,
    pub is_dq: bool,
    pub games: Vec<GameReport>,
}

struct EventFixture {
    structure: get_event_structure::Event,
    /// Successive poll results; the cursor advances per fetch and repeats the
    /// last snapshot once the script runs out.
    snapshots: Vec<Vec<get_sets_for_event::Set>>,
    cursor: usize,
    /// Paced mode: wall-millisecond release offsets parallel to `snapshots`.
    /// When set, fetches select by elapsed time on the source's shared clock
    /// instead of advancing the cursor.
    offsets: Option<Vec<i64>>,
    /// When set, fetches for this event never resolve (wedged-host fixture).
    hang: bool,
}

#[derive(Default)]
pub struct FixtureSource {
    events: Mutex<HashMap<String, EventFixture>>,
    /// Real per-event rosters (`--rehearse` seeds them from the live API);
    /// events without one answer the FIXTURE_ROSTER placeholder.
    rosters: Mutex<HashMap<String, Vec<CharacterInfo>>>,
    mutations: Mutex<Vec<MutationRecord>>,
    reports: Mutex<Vec<ReportRecord>>,
    /// Scripted admin-probe answer; unset answers as a full admin (id 1) so
    /// writes-armed fixtures keep working.
    admin_probe: Mutex<Option<AdminProbeResult>>,
    /// The shared rehearsal clock, armed by the first paced fetch. One clock
    /// across all events keeps their timelines in lockstep.
    clock: Mutex<Option<Instant>>,
}

impl FixtureSource {
    pub fn new() -> Self {
        Self::default()
    }

    /// Loads every `tournament_*` event directory of an S1-smoke-style
    /// capture dir: `sets_page_N.json` pages (one combined initial snapshot)
    /// plus `structure.json`, all raw `GraphQlResponse` envelopes.
    pub fn from_captures(dir: &Path) -> Result<Self, FixtureLoadError> {
        let mut source = Self::new();
        let entries = fs::read_dir(dir).map_err(|source| FixtureLoadError::Io {
            path: dir.to_owned(),
            source,
        })?;
        let mut loaded = 0;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !entry.path().is_dir() || !name.starts_with("tournament_") {
                continue;
            }
            source.load_capture_event(&entry.path(), &name)?;
            loaded += 1;
        }
        if loaded == 0 {
            return Err(FixtureLoadError::NoEvents { dir: dir.to_owned() });
        }
        Ok(source)
    }

    /// Builds a purely synthetic world from a `--synth` spec: comma-separated
    /// `kind:entrants` entries (`de:32`, `se:16`, `rr:8`, `swiss:16` — swiss
    /// takes an optional `:rounds`). Adjacent events share ~half their
    /// players, so cross-event conflicts are real. Set ids are numeric from
    /// the start, so calls exercise the full write path even without
    /// `--pace`.
    pub fn from_synth_spec(spec: &str) -> Result<Self, SynthSpecError> {
        let mut source = Self::new();
        let entries: Vec<SynthEntry> = spec
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .map(parse_synth_entry)
            .collect::<Result<_, _>>()?;
        if entries.is_empty() {
            return Err(SynthSpecError::Empty);
        }

        // One shared pool, each event's slice starting halfway into its
        // predecessor's.
        let mut starts = Vec::with_capacity(entries.len());
        let mut next_start = 0usize;
        let mut pool_len = 0usize;
        for entry in &entries {
            starts.push(next_start);
            pool_len = pool_len.max(next_start + entry.entrants);
            next_start += entry.entrants.div_ceil(2);
        }
        let pool = tagged_players(pool_len);

        for (index, entry) in entries.iter().enumerate() {
            let players = &pool[starts[index]..starts[index] + entry.entrants];
            let pg = SYNTH_PG_BASE + index as u64;
            let bracket = match entry.kind {
                SynthKind::De => make_de_bracket_with(pg, players),
                SynthKind::Se => make_se_bracket_with(pg, players),
                SynthKind::Rr => make_rr_pool_with(pg, players),
                SynthKind::Swiss => make_swiss_with(pg, players, entry.rounds),
            };
            let slug = format!("{SYNTH_TOURNAMENT}/event/{}{}-{}", entry.kind.token(), entry.entrants, index + 1);
            let sets = materialize_ids(&bracket.sets, SYNTH_ID_BASE + index as u64 * SYNTH_ID_STRIDE);
            source.add_synth_event(&slug, from_ref(&bracket.info), vec![sets]);
        }
        Ok(source)
    }

    /// Registers an event under `slug` with a scripted snapshot sequence of
    /// model-layer sets (converted to schema shape here, so fetches still
    /// exercise the real forward conversion).
    pub fn add_synth_event(&mut self, slug: &str, groups: &[PhaseGroupInfo], snapshots: Vec<Vec<LiveSet>>) {
        assert!(!snapshots.is_empty(), "an event needs at least one snapshot");
        let num_entrants = distinct_entrants(&snapshots[0]);
        let structure = schema_structure_from_groups(slug, groups, num_entrants);
        let snapshots = snapshots
            .iter()
            .map(|snapshot| snapshot.iter().map(schema_set_from_live).collect())
            .collect();
        self.add_schema_event(slug, structure, snapshots);
    }

    /// Registers an event whose snapshots are already schema-layer sets.
    pub fn add_schema_event(&mut self, slug: &str, structure: get_event_structure::Event, snapshots: Vec<Vec<get_sets_for_event::Set>>) {
        assert!(!snapshots.is_empty(), "an event needs at least one snapshot");
        self.events.get_mut().unwrap().insert(
            capture_key(slug),
            EventFixture {
                structure,
                snapshots,
                cursor: 0,
                offsets: None,
                hang: false,
            },
        );
    }

    /// Switches a registered event to paced replay: each `(offset_ms, sets)`
    /// entry releases once that much wall time has elapsed on the shared
    /// clock. Also clears the structure's `start_at` — a paced world is live
    /// *now*, not at the captured start time.
    pub fn set_timeline(&mut self, slug: &str, timeline: Vec<(i64, Vec<get_sets_for_event::Set>)>) {
        assert!(!timeline.is_empty(), "a timeline needs at least one frame");
        let events = self.events.get_mut().unwrap();
        let fixture = events.get_mut(&capture_key(slug)).expect("timeline target must be registered");
        fixture.structure.start_at = None;
        let (offsets, snapshots) = timeline.into_iter().unzip();
        fixture.offsets = Some(offsets);
        fixture.snapshots = snapshots;
        fixture.cursor = 0;
    }

    /// Moves the shared clock so `elapsed` has already passed — tests and
    /// rehearsal dry-runs fast-forward without sleeping.
    #[doc(hidden)]
    pub fn rewind_clock(&self, elapsed: Duration) {
        let started = Instant::now().checked_sub(elapsed).expect("rewind within the process epoch");
        *self.clock.lock().unwrap() = Some(started);
    }

    /// Elapsed wall milliseconds on the shared clock, arming it on first use.
    fn elapsed_ms(&self) -> i64 {
        let mut clock = self.clock.lock().unwrap();
        clock.get_or_insert_with(Instant::now).elapsed().as_millis() as i64
    }

    /// Makes every fetch for `slug` hang forever (wedged-host fixture).
    /// Installs an event's real character roster; its `fetch_event_characters`
    /// answers this instead of the placeholder.
    pub fn set_event_roster(&mut self, slug: &str, roster: Vec<CharacterInfo>) {
        self.rosters.lock().unwrap().insert(capture_key(slug), roster);
    }

    pub fn set_hang(&mut self, slug: &str) {
        self.events
            .get_mut()
            .unwrap()
            .get_mut(&capture_key(slug))
            .expect("hang target must be registered")
            .hang = true;
    }

    /// Every write answered so far, in arrival order.
    pub fn mutation_log(&self) -> Vec<MutationRecord> {
        self.mutations.lock().unwrap().clone()
    }

    /// Every set report answered so far, in arrival order.
    pub fn report_log(&self) -> Vec<ReportRecord> {
        self.reports.lock().unwrap().clone()
    }

    /// True when `slug` names a registered event (live form or capture-dir
    /// name — the same keys fetches accept).
    pub fn has_event(&self, slug: &str) -> bool {
        self.events.lock().unwrap().contains_key(&capture_key(slug))
    }

    /// Registered event slugs in live form (`tournament/x/event/y`), sorted.
    pub fn event_slugs(&self) -> Vec<String> {
        let mut slugs: Vec<String> = self.events.lock().unwrap().keys().map(|key| live_slug(key)).collect();
        slugs.sort();
        slugs
    }

    /// A ready-to-run config for a zero-config `--simulate` run: the largest
    /// captured tournament's events sharing a synthetic setup pool, writes
    /// left armed (this source answers mutations), state ints pinned to the
    /// fixture's answers. Returns the config plus the events skipped for
    /// belonging to other tournaments (preflight's identity assertion allows
    /// only one).
    pub fn derived_config(&self) -> (SchedulerConfig, Vec<String>) {
        let slugs = self.event_slugs();
        let chosen_tournament = largest_tournament(&slugs);
        let (chosen, skipped): (Vec<String>, Vec<String>) =
            slugs.into_iter().partition(|slug| tournament_prefix(slug) == chosen_tournament);

        let brackets = chosen.iter().map(|slug: &String| BracketConfig::new(slug.clone())).collect();
        let config = SchedulerConfig {
            brackets,
            setups: Some(SetupCounts::Uniform(SIM_SETUP_COUNT)),
            known_called_state_int: Some(FIXTURE_CALLED_INT),
            known_in_progress_state_int: Some(FIXTURE_IN_PROGRESS_INT),
            ..SchedulerConfig::default()
        };
        (config, skipped)
    }

    /// Scripts the admin probe's answer (e.g. a non-admin token).
    pub fn set_admin_probe(&mut self, result: AdminProbeResult) {
        *self.admin_probe.get_mut().unwrap() = Some(result);
    }

    fn load_capture_event(&mut self, event_dir: &Path, name: &str) -> Result<(), FixtureLoadError> {
        let mut sets = Vec::new();
        for page in 1.. {
            let path = event_dir.join(format!("sets_page_{page}.json"));
            if !path.exists() {
                break;
            }
            let response: GraphQlResponse<get_sets_for_event::GetSetsForEvent> = read_envelope(&path)?;
            let nodes = response
                .data
                .and_then(|d| d.event)
                .and_then(|e| e.sets)
                .and_then(|s| s.nodes)
                .ok_or(FixtureLoadError::EmptyEnvelope { path })?;
            sets.extend(nodes.into_iter().flatten());
        }

        let structure_path = event_dir.join("structure.json");
        let response: GraphQlResponse<get_event_structure::GetEventStructure> = read_envelope(&structure_path)?;
        let structure = response
            .data
            .and_then(|d| d.event)
            .ok_or(FixtureLoadError::EmptyEnvelope { path: structure_path })?;

        self.add_schema_event(name, structure, vec![sets]);
        Ok(())
    }

    fn record_mutation(&self, kind: MutationKind, id: StartGgId, state: i32) -> SetMutationResult {
        self.mutations.lock().unwrap().push(MutationRecord { kind, id });
        SetMutationResult {
            id: Some(id),
            state: Some(state),
            started_at: None,
            completed_at: None,
        }
    }
}

impl SetSource for FixtureSource {
    type Error = FixtureError;

    async fn fetch_event_sets(&self, event_slug: &str) -> Result<Vec<get_sets_for_event::Set>, FixtureError> {
        let snapshot = {
            let mut events = self.events.lock().unwrap();
            let fixture = events
                .get_mut(&capture_key(event_slug))
                .ok_or_else(|| FixtureError::UnknownEvent(event_slug.to_owned()))?;
            if fixture.hang {
                None
            } else if let Some(offsets) = &fixture.offsets {
                let elapsed = self.elapsed_ms();
                let due = offsets.iter().rposition(|&o| o <= elapsed).unwrap_or(0);
                Some(fixture.snapshots[due].clone())
            } else {
                let snapshot = fixture.snapshots[fixture.cursor].clone();
                fixture.cursor = (fixture.cursor + 1).min(fixture.snapshots.len() - 1);
                Some(snapshot)
            }
        };
        match snapshot {
            Some(snapshot) => Ok(snapshot),
            None => pending().await,
        }
    }

    async fn fetch_event_structure(&self, event_slug: &str) -> Result<get_event_structure::Event, FixtureError> {
        let structure = {
            let events = self.events.lock().unwrap();
            let fixture = events
                .get(&capture_key(event_slug))
                .ok_or_else(|| FixtureError::UnknownEvent(event_slug.to_owned()))?;
            if fixture.hang {
                None
            } else {
                Some(fixture.structure.clone())
            }
        };
        match structure {
            Some(structure) => Ok(structure),
            None => pending().await,
        }
    }

    async fn mark_called(&self, set_id: StartGgId) -> Result<SetMutationResult, FixtureError> {
        Ok(self.record_mutation(MutationKind::Called, set_id, FIXTURE_CALLED_INT))
    }

    async fn mark_in_progress(&self, set_id: StartGgId) -> Result<SetMutationResult, FixtureError> {
        Ok(self.record_mutation(MutationKind::InProgress, set_id, FIXTURE_IN_PROGRESS_INT))
    }

    async fn probe_admin(&self, _tournament_id: StartGgId) -> Result<AdminProbeResult, FixtureError> {
        Ok(self.admin_probe.lock().unwrap().clone().unwrap_or(AdminProbeResult {
            current_user: Some(1),
            admins: Some(vec![1]),
        }))
    }

    async fn fetch_event_characters(&self, event_slug: &str) -> Result<Vec<CharacterInfo>, FixtureError> {
        if !self.events.lock().unwrap().contains_key(&capture_key(event_slug)) {
            return Err(FixtureError::UnknownEvent(event_slug.to_owned()));
        }
        if let Some(roster) = self.rosters.lock().unwrap().get(&capture_key(event_slug)) {
            return Ok(roster.clone());
        }
        Ok(FIXTURE_ROSTER
            .iter()
            .enumerate()
            .map(|(ix, name)| CharacterInfo {
                id: ix as i32 + 1,
                name: (*name).to_owned(),
            })
            .collect())
    }

    async fn report_set(
        &self,
        set_id: StartGgId,
        winner_entrant_id: Option<String>,
        is_dq: bool,
        games: Vec<GameReport>,
    ) -> Result<SetMutationResult, FixtureError> {
        self.reports.lock().unwrap().push(ReportRecord {
            set_id,
            winner_entrant_id,
            is_dq,
            games,
        });
        let completed_at = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
        Ok(SetMutationResult {
            id: Some(set_id),
            state: Some(FIXTURE_COMPLETED_INT),
            started_at: None,
            completed_at: Some(Timestamp(completed_at)),
        })
    }
}

/// Three-bucket classification for fixture errors (`--simulate` runs): an
/// unknown event is a config typo — definitively wrong, never retried.
pub fn classify_fixture_error(error: &FixtureError) -> crate::app::PollFailure {
    crate::app::PollFailure::Persistent(error.to_string())
}

/// Event lookups tolerate both slug forms: the live `tournament/x/event/y`
/// and the capture directory's `tournament_x_event_y`.
fn capture_key(slug: &str) -> String {
    slug.replace('/', "_")
}

/// Inverse of [`capture_key`]: reconstructs the live slug from a capture key.
/// Underscores *inside* the tournament or event slug survive (only the two
/// separators convert), keyed on the first `_event_` occurrence. Keys that
/// don't match the pattern pass through untouched.
fn live_slug(capture_key: &str) -> String {
    let Some(rest) = capture_key.strip_prefix("tournament_") else {
        return capture_key.to_owned();
    };
    match rest.split_once("_event_") {
        Some((tournament, event)) => format!("tournament/{tournament}/event/{event}"),
        None => capture_key.to_owned(),
    }
}

/// The `tournament/<t>` prefix of a live event slug.
fn tournament_prefix(slug: &str) -> &str {
    slug.split("/event/").next().unwrap_or(slug)
}

#[derive(Debug, Clone, Copy)]
enum SynthKind {
    De,
    Se,
    Rr,
    Swiss,
}

impl SynthKind {
    fn token(self) -> &'static str {
        match self {
            Self::De => "de",
            Self::Se => "se",
            Self::Rr => "rr",
            Self::Swiss => "swiss",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct SynthEntry {
    kind: SynthKind,
    entrants: usize,
    /// Swiss only; other kinds derive their own round structure.
    rounds: i32,
}

fn parse_synth_entry(entry: &str) -> Result<SynthEntry, SynthSpecError> {
    let bad = |reason: &str| SynthSpecError::Bad {
        entry: entry.to_owned(),
        reason: reason.to_owned(),
    };

    let mut parts = entry.split(':');
    let kind = match parts.next() {
        Some("de") => SynthKind::De,
        Some("se") => SynthKind::Se,
        Some("rr") => SynthKind::Rr,
        Some("swiss") => SynthKind::Swiss,
        _ => return Err(bad("unknown kind")),
    };
    let entrants: usize = parts
        .next()
        .ok_or_else(|| bad("missing entrant count"))?
        .parse()
        .map_err(|_| bad("entrant count is not a number"))?;
    if entrants < 2 {
        return Err(bad("needs at least 2 entrants"));
    }
    let rounds = match (parts.next(), kind) {
        (None, _) => default_swiss_rounds(entrants),
        (Some(raw), SynthKind::Swiss) => raw.parse().map_err(|_| bad("rounds is not a number"))?,
        (Some(_), _) => return Err(bad("only swiss takes a :rounds part")),
    };
    if parts.next().is_some() {
        return Err(bad("too many parts"));
    }
    Ok(SynthEntry { kind, entrants, rounds })
}

/// The usual swiss schedule: enough rounds to separate the field.
fn default_swiss_rounds(entrants: usize) -> i32 {
    (usize::BITS - entrants.next_power_of_two().leading_zeros() - 1).max(1) as i32
}

/// The tournament with the most events; ties break lexicographically so the
/// derived config is deterministic.
fn largest_tournament(slugs: &[String]) -> String {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for slug in slugs {
        *counts.entry(tournament_prefix(slug)).or_default() += 1;
    }
    counts
        .into_iter()
        .max_by(|(a_name, a_count), (b_name, b_count)| a_count.cmp(b_count).then(b_name.cmp(a_name)))
        .map(|(name, _)| name.to_owned())
        .unwrap_or_default()
}

fn read_envelope<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, FixtureLoadError> {
    let raw = fs::read_to_string(path).map_err(|source| FixtureLoadError::Io {
        path: path.to_owned(),
        source,
    })?;
    serde_json::from_str(&raw).map_err(|source| FixtureLoadError::Parse {
        path: path.to_owned(),
        source: Box::new(source),
    })
}

fn distinct_entrants(sets: &[LiveSet]) -> i32 {
    let entrants: HashSet<_> = sets.iter().flat_map(LiveSet::occupants).map(|o| &o.entrant_id).collect();
    entrants.len() as i32
}

/// Inverse of `live_set_from_schema`, for synthetic fixtures. One lossy
/// corner: the schema's `winner_id` is numeric, so non-numeric synthetic
/// entrant ids drop it — completion still round-trips via `completed_at`
/// (which `synth::complete` always sets).
pub fn schema_set_from_live(set: &LiveSet) -> get_sets_for_event::Set {
    get_sets_for_event::Set {
        id: Some(Id::new(set.id.0.clone())),
        state: set.state_int,
        round: Some(set.key.round),
        identifier: Some(set.key.identifier.clone()),
        full_round_text: set.full_round_text.clone(),
        started_at: set.started_at.map(Timestamp),
        completed_at: set.completed_at.map(Timestamp),
        winner_id: set.winner_id.as_ref().and_then(|w| w.0.parse().ok()),
        has_placeholder: Some(set.has_placeholder),
        phase_group: Some(get_sets_for_event::PhaseGroup {
            id: Some(Id::new(set.key.phase_group.clone())),
        }),
        slots: Some(set.slots.iter().enumerate().map(|(i, slot)| Some(schema_slot(i, slot))).collect()),
    }
}

fn schema_slot(index: usize, slot: &Slot) -> get_sets_for_event::SetSlot {
    let (prereq_id, prereq_type, prereq_placement) = match &slot.prereq {
        Some(Prereq::Set { id, placement }) => (Some(id.0.clone()), Some(PREREQ_TYPE_SET.to_owned()), *placement),
        Some(Prereq::PreSatisfied { raw_type }) => (None, Some(raw_type.clone().unwrap_or_else(|| PREREQ_TYPE_SEED.to_owned())), None),
        None => (None, None, None),
    };
    let entrant = slot.occupant.as_ref().map(|occupant| get_sets_for_event::Entrant {
        id: Some(Id::new(occupant.entrant_id.0.clone())),
        name: Some(occupant.display_name.clone()),
        is_disqualified: Some(occupant.is_disqualified),
        participants: Some(
            occupant
                .player_ids
                .iter()
                .map(|player_id| {
                    Some(get_sets_for_event::Participant {
                        gamer_tag: Some(occupant.display_name.clone()),
                        player: Some(get_sets_for_event::Player {
                            id: Some(Id::new(player_id.0.clone())),
                        }),
                    })
                })
                .collect(),
        ),
    });
    get_sets_for_event::SetSlot {
        slot_index: Some(index as i32),
        prereq_id,
        prereq_type,
        prereq_placement,
        entrant,
    }
}

/// Fabricates the structure envelope a synthetic event would have returned.
/// Tournament identity is derived from the slug's `tournament/...` prefix so
/// same-tournament events agree (the preflight identity assertion).
pub fn schema_structure_from_groups(event_slug: &str, groups: &[PhaseGroupInfo], num_entrants: i32) -> get_event_structure::Event {
    let tournament_slug = event_slug.split("/event/").next().unwrap_or(event_slug).to_owned();
    get_event_structure::Event {
        id: Some(Id::new(format!("synth-{}", capture_key(event_slug)))),
        name: Some(event_slug.to_owned()),
        state: Some(ActivityState::Active),
        start_at: None,
        tournament: Some(get_event_structure::Tournament {
            // Live tournament ids are numeric (the admin probe parses them),
            // so the synthetic id is a stable hash of the slug.
            id: Some(Id::new(synth_tournament_id(&tournament_slug).to_string())),
            slug: Some(tournament_slug),
        }),
        phases: Some(vec![Some(get_event_structure::Phase {
            id: Some(Id::new(format!("synth-phase-{}", capture_key(event_slug)))),
            state: Some(ActivityState::Active),
        })]),
        phase_groups: Some(groups.iter().map(|g| Some(schema_phase_group(g))).collect()),
        num_entrants: Some(num_entrants),
    }
}

/// FNV-1a over the slug: deterministic, distinct per tournament.
fn synth_tournament_id(slug: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in slug.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn schema_phase_group(info: &PhaseGroupInfo) -> get_event_structure::PhaseGroup {
    let bracket_type = match &info.kind {
        GroupKind::Elimination => Some(BracketType::DoubleElimination),
        GroupKind::RoundRobin => Some(BracketType::RoundRobin),
        GroupKind::Swiss { .. } => Some(BracketType::Swiss),
        GroupKind::Unsupported(_) => None,
    };
    let num_rounds = match info.kind {
        GroupKind::Swiss { num_rounds } => Some(num_rounds),
        _ => info.num_rounds,
    };
    let rounds = (!info.best_of_by_round.is_empty()).then(|| {
        info.best_of_by_round
            .iter()
            .map(|(number, best_of)| {
                Some(get_event_structure::Round {
                    number: Some(*number),
                    best_of: Some(*best_of),
                })
            })
            .collect()
    });
    get_event_structure::PhaseGroup {
        id: Some(Id::new(info.id.clone())),
        bracket_type,
        num_rounds,
        start_at: info.start_at.map(Timestamp),
        wave: None,
        rounds,
    }
}

#[cfg(test)]
mod tests {
    use std::{env, path::PathBuf, slice::from_ref, time::Duration};

    use bracket_tools_startgg_schema::{get_sets_for_event, scalars::Timestamp};
    use tokio::time::timeout;

    use super::{
        capture_key, live_slug, schema_set_from_live, FixtureSource, MutationKind, MutationRecord, SynthSpecError, FIXTURE_CALLED_INT,
        FIXTURE_IN_PROGRESS_INT, SIM_SETUP_COUNT,
    };
    use crate::{
        config::SetupCounts,
        model::{live_sets_from_schema, phase_groups_from_schema, LiveSet},
        set_source::SetSource,
        synth::{complete, make_de_bracket, make_swiss},
    };

    const SLUG: &str = "tournament/synth/event/melee-singles";

    fn de_source() -> FixtureSource {
        let bracket = make_de_bracket(1001, 8);
        let mut source = FixtureSource::new();
        source.add_synth_event(SLUG, from_ref(&bracket.info), vec![bracket.sets.clone()]);
        source
    }

    #[tokio::test]
    async fn synth_sets_round_trip_through_schema_conversion() {
        let bracket = make_de_bracket(1001, 8);
        let source = de_source();

        let schema_sets = source.fetch_event_sets(SLUG).await.unwrap();
        let (live, warnings, skipped) = live_sets_from_schema(schema_sets);

        assert!(skipped.is_empty(), "{skipped:?}");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(live, bracket.sets);
    }

    #[tokio::test]
    async fn synth_structure_round_trips_through_schema_conversion() {
        let bracket = make_de_bracket(1001, 8);
        let source = de_source();

        let structure = source.fetch_event_structure(SLUG).await.unwrap();
        let (groups, warnings) = phase_groups_from_schema(&structure);

        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(groups, vec![bracket.info]);
        let tournament = structure.tournament.unwrap();
        assert_eq!(tournament.slug.as_deref(), Some("tournament/synth"));
    }

    #[tokio::test]
    async fn swiss_structure_round_trips_num_rounds() {
        let swiss = make_swiss(2002, 8, 4);
        let mut source = FixtureSource::new();
        source.add_synth_event("tournament/synth/event/pokemon", from_ref(&swiss.info), vec![swiss.sets]);

        let structure = source.fetch_event_structure("tournament/synth/event/pokemon").await.unwrap();
        let (groups, _) = phase_groups_from_schema(&structure);
        assert_eq!(groups, vec![swiss.info]);
    }

    #[tokio::test]
    async fn snapshot_sequence_advances_then_repeats_last() {
        let bracket = make_de_bracket(1001, 4);
        let mut second = bracket.sets.clone();
        complete(&mut second[0], 0, 1_751_000_100);

        let mut source = FixtureSource::new();
        source.add_synth_event(SLUG, from_ref(&bracket.info), vec![bracket.sets.clone(), second.clone()]);

        let expect_completed = |sets: Vec<bracket_tools_startgg_schema::get_sets_for_event::Set>, expected: usize| {
            let (live, _, _) = live_sets_from_schema(sets);
            assert_eq!(live.iter().filter(|s| s.is_completed()).count(), expected);
        };

        expect_completed(source.fetch_event_sets(SLUG).await.unwrap(), 0);
        expect_completed(source.fetch_event_sets(SLUG).await.unwrap(), 1);
        // Script exhausted: the last snapshot repeats.
        expect_completed(source.fetch_event_sets(SLUG).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn paced_timeline_releases_by_elapsed_wall_time() {
        let bracket = make_de_bracket(1001, 4);
        let mut second = bracket.sets.clone();
        complete(&mut second[0], 0, 1_751_000_100);
        let mut third = second.clone();
        complete(&mut third[1], 0, 1_751_000_200);

        let to_schema = |sets: &[LiveSet]| sets.iter().map(schema_set_from_live).collect::<Vec<_>>();
        let mut source = FixtureSource::new();
        source.add_synth_event(SLUG, from_ref(&bracket.info), vec![bracket.sets.clone()]);
        source.set_timeline(
            SLUG,
            vec![
                (0, to_schema(&bracket.sets)),
                (60_000, to_schema(&second)),
                (120_000, to_schema(&third)),
            ],
        );

        let completed = |sets: Vec<get_sets_for_event::Set>| {
            let (live, _, _) = live_sets_from_schema(sets);
            live.iter().filter(|s| s.is_completed()).count()
        };

        // Before the first offset elapses, every fetch serves frame 0.
        assert_eq!(completed(source.fetch_event_sets(SLUG).await.unwrap()), 0);
        assert_eq!(completed(source.fetch_event_sets(SLUG).await.unwrap()), 0);

        source.rewind_clock(Duration::from_secs(61));
        assert_eq!(completed(source.fetch_event_sets(SLUG).await.unwrap()), 1);

        // Past the end of the script, the last frame repeats.
        source.rewind_clock(Duration::from_secs(600));
        assert_eq!(completed(source.fetch_event_sets(SLUG).await.unwrap()), 2);
        assert_eq!(completed(source.fetch_event_sets(SLUG).await.unwrap()), 2);
    }

    #[tokio::test]
    async fn paced_timeline_clears_structure_start() {
        let bracket = make_de_bracket(1001, 4);
        let mut source = FixtureSource::new();
        source.add_synth_event(SLUG, from_ref(&bracket.info), vec![bracket.sets.clone()]);
        source
            .events
            .get_mut()
            .unwrap()
            .get_mut(&super::capture_key(SLUG))
            .unwrap()
            .structure
            .start_at = Some(Timestamp(1_800_000_000));

        source.set_timeline(SLUG, vec![(0, bracket.sets.iter().map(schema_set_from_live).collect())]);

        let structure = source.fetch_event_structure(SLUG).await.unwrap();
        assert_eq!(structure.start_at, None, "a paced world is live now");
    }

    #[tokio::test]
    async fn unknown_event_errors() {
        let source = de_source();
        assert!(source.fetch_event_sets("tournament/other/event/x").await.is_err());
        assert!(source.fetch_event_structure("tournament/other/event/x").await.is_err());
    }

    #[tokio::test]
    async fn hanging_event_never_resolves() {
        let mut source = de_source();
        source.set_hang(SLUG);

        let fetch = timeout(Duration::from_millis(20), source.fetch_event_sets(SLUG));
        assert!(fetch.await.is_err(), "hang fixture must outlive the timeout");
        let structure = timeout(Duration::from_millis(20), source.fetch_event_structure(SLUG));
        assert!(structure.await.is_err());
    }

    #[tokio::test]
    async fn mutations_are_recorded_and_answered() {
        let source = de_source();

        let result = source.mark_called(4242).await.unwrap();
        assert_eq!(result.id, Some(4242));
        assert_eq!(result.state, Some(FIXTURE_CALLED_INT));
        source.mark_in_progress(4242).await.unwrap();

        assert_eq!(
            source.mutation_log(),
            vec![
                MutationRecord {
                    kind: MutationKind::Called,
                    id: 4242
                },
                MutationRecord {
                    kind: MutationKind::InProgress,
                    id: 4242
                },
            ]
        );
    }

    #[test]
    fn live_slug_round_trips_capture_keys() {
        assert_eq!(
            live_slug("tournament_french-bread-rumble-100_event_ultimate-singles"),
            "tournament/french-bread-rumble-100/event/ultimate-singles"
        );
        // Underscores inside the tournament slug survive.
        assert_eq!(
            live_slug("tournament_rust_vitational_mk_xiii_event_ultimate-singles"),
            "tournament/rust_vitational_mk_xiii/event/ultimate-singles"
        );
        assert_eq!(live_slug("not-a-capture-key"), "not-a-capture-key");
    }

    #[test]
    fn event_slugs_are_live_form_and_sorted() {
        let source = de_source();
        assert_eq!(source.event_slugs(), vec![SLUG.to_owned()]);
    }

    #[test]
    fn has_event_accepts_live_slugs_and_capture_keys() {
        let source = de_source();
        assert!(source.has_event(SLUG));
        assert!(source.has_event(&capture_key(SLUG)));
        assert!(!source.has_event("tournament/your-tournament/event/your-main-event"));
    }

    #[test]
    fn synth_spec_builds_a_world_with_numeric_ids() {
        let source = FixtureSource::from_synth_spec("de:8, rr:4, swiss:8:3").unwrap();
        assert_eq!(
            source.event_slugs(),
            vec![
                "tournament/synth/event/de8-1",
                "tournament/synth/event/rr4-2",
                "tournament/synth/event/swiss8-3",
            ]
        );
    }

    #[tokio::test]
    async fn synth_spec_sets_convert_cleanly() {
        let source = FixtureSource::from_synth_spec("de:8").unwrap();
        let sets = source.fetch_event_sets("tournament/synth/event/de8-1").await.unwrap();
        let (live, warnings, skipped) = live_sets_from_schema(sets);
        assert!(skipped.is_empty(), "{skipped:?}");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(!live.is_empty());
        assert!(live.iter().all(|set| set.id.0.parse::<u64>().is_ok()), "synth ids are numeric");
    }

    #[test]
    fn synth_spec_multi_event_is_one_tournament() {
        let source = FixtureSource::from_synth_spec("de:32,de:16,rr:6,swiss:16").unwrap();
        let (config, skipped) = source.derived_config();
        assert_eq!(config.brackets.len(), 4);
        assert!(skipped.is_empty(), "{skipped:?}");
        config.validate().unwrap();
    }

    #[test]
    fn synth_spec_rejects_the_removed_fbr_literal() {
        assert!(matches!(FixtureSource::from_synth_spec("fbr"), Err(SynthSpecError::Bad { .. })));
    }

    #[tokio::test]
    async fn synth_players_wear_tags_not_numbers() {
        let source = FixtureSource::from_synth_spec("de:8").unwrap();
        let sets = source.fetch_event_sets("tournament/synth/event/de8-1").await.unwrap();
        let (live, _, _) = live_sets_from_schema(sets);
        let names: Vec<&str> = live
            .iter()
            .flat_map(|s| s.slots.iter())
            .filter_map(|slot| slot.occupant.as_ref().map(|o| o.display_name.as_str()))
            .collect();
        assert!(!names.is_empty());
        assert!(names.iter().all(|n| !n.starts_with("Player ")), "tags expected: {names:?}");
    }

    #[test]
    fn synth_spec_rejects_garbage() {
        for bad in ["", "  ,  ", "melee:8", "de:one", "de:1", "de:8:3", "swiss:8:x"] {
            assert!(matches!(
                FixtureSource::from_synth_spec(bad),
                Err(SynthSpecError::Empty | SynthSpecError::Bad { .. })
            ));
        }
    }

    #[test]
    fn derived_config_picks_the_largest_tournament() {
        let de = make_de_bracket(1001, 4);
        let mut source = FixtureSource::new();
        source.add_synth_event("tournament/big/event/melee", from_ref(&de.info), vec![de.sets.clone()]);
        source.add_synth_event("tournament/big/event/ultimate", from_ref(&de.info), vec![de.sets.clone()]);
        source.add_synth_event("tournament/small/event/rivals", from_ref(&de.info), vec![de.sets.clone()]);

        let (config, skipped) = source.derived_config();
        config.validate().unwrap();
        let slugs: Vec<_> = config.brackets.iter().map(|b| b.slug.as_str()).collect();
        assert_eq!(slugs, vec!["tournament/big/event/melee", "tournament/big/event/ultimate"]);
        assert_eq!(skipped, vec!["tournament/small/event/rivals".to_owned()]);
        assert_eq!(config.setups, Some(SetupCounts::Uniform(SIM_SETUP_COUNT)));
        assert!(
            config.brackets.iter().all(|b| b.setup_types() == vec!["default"]),
            "one shared pool"
        );
        assert_eq!(config.known_called_state_int, Some(FIXTURE_CALLED_INT));
        assert_eq!(config.known_in_progress_state_int, Some(FIXTURE_IN_PROGRESS_INT));
        assert!(!config.advisor_only, "derived sim sessions arm writes");
    }

    /// Env-gated like tests/fixture_replay.rs: exercises the capture loader
    /// against the real S1 corpus when it's present.
    #[tokio::test]
    async fn captures_load_and_serve_by_live_slug() {
        let dir = match env::var("BRACKET_TOOLS_CAPTURES") {
            Ok(dir) => PathBuf::from(dir),
            Err(_) => {
                let Ok(home) = env::var("HOME") else { return };
                PathBuf::from(home).join("work/personal/bracket-tools-captures/2026-07-05_s1_smoke")
            }
        };
        if !dir.is_dir() {
            eprintln!("skipping capture-loader test: {} absent", dir.display());
            return;
        }

        let source = FixtureSource::from_captures(&dir).unwrap();
        let slug = "tournament/french-bread-rumble-100/event/ultimate-singles";
        let sets = source.fetch_event_sets(slug).await.unwrap();
        let (live, _, skipped) = live_sets_from_schema(sets);
        assert!(skipped.is_empty());
        assert_eq!(live.len(), 135, "the S1 FBR ultimate skeleton");

        let structure = source.fetch_event_structure(slug).await.unwrap();
        let (groups, warnings) = phase_groups_from_schema(&structure);
        assert!(warnings.is_empty());
        assert!(!groups.is_empty());
    }
}

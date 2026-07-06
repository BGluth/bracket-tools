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
    sync::Mutex,
};

use bracket_tools_startgg::{SetMutationResult, StartGgId};
use bracket_tools_startgg_schema::{
    enums::{ActivityState, BracketType},
    get_event_structure, get_sets_for_event,
    scalars::{Id, Timestamp},
};
use cynic::GraphQlResponse;
use thiserror::Error;

use crate::{
    model::{GroupKind, LiveSet, PhaseGroupInfo, Prereq, Slot, PREREQ_TYPE_SEED, PREREQ_TYPE_SET},
    set_source::SetSource,
};

/// The state ints the fixture answers mutations with, matching live
/// observations (CALLED=6, IN_PROGRESS=2).
pub const FIXTURE_CALLED_INT: i32 = 6;
pub const FIXTURE_IN_PROGRESS_INT: i32 = 2;

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

struct EventFixture {
    structure: get_event_structure::Event,
    /// Successive poll results; the cursor advances per fetch and repeats the
    /// last snapshot once the script runs out.
    snapshots: Vec<Vec<get_sets_for_event::Set>>,
    cursor: usize,
    /// When set, fetches for this event never resolve (wedged-host fixture).
    hang: bool,
}

#[derive(Default)]
pub struct FixtureSource {
    events: Mutex<HashMap<String, EventFixture>>,
    mutations: Mutex<Vec<MutationRecord>>,
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
                hang: false,
            },
        );
    }

    /// Makes every fetch for `slug` hang forever (wedged-host fixture).
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
                .ok_or_else(|| FixtureLoadError::EmptyEnvelope { path })?;
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
}

/// Event lookups tolerate both slug forms: the live `tournament/x/event/y`
/// and the capture directory's `tournament_x_event_y`.
fn capture_key(slug: &str) -> String {
    slug.replace('/', "_")
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
            id: Some(Id::new(tournament_slug.clone())),
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
    use std::{env, path::PathBuf, time::Duration};

    use tokio::time::timeout;

    use super::{FixtureSource, MutationKind, MutationRecord, FIXTURE_CALLED_INT};
    use crate::{
        model::{live_sets_from_schema, phase_groups_from_schema},
        set_source::SetSource,
        synth::{complete, make_de_bracket, make_swiss},
    };

    const SLUG: &str = "tournament/synth/event/melee-singles";

    fn de_source() -> FixtureSource {
        let bracket = make_de_bracket(1001, 8);
        let mut source = FixtureSource::new();
        source.add_synth_event(SLUG, &[bracket.info.clone()], vec![bracket.sets.clone()]);
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
        source.add_synth_event("tournament/synth/event/pokemon", &[swiss.info.clone()], vec![swiss.sets]);

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
        source.add_synth_event(SLUG, &[bracket.info.clone()], vec![bracket.sets.clone(), second.clone()]);

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

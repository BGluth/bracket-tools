//! Scheduler-local set model: owned, nullability-tolerant types converted from
//! the schema layer once per poll, so the rest of the core never touches
//! `Option`-riddled cynic types.

use std::collections::BTreeMap;

use bracket_tools_startgg_schema::{enums::BracketType, get_event_structure, get_sets_for_event};
use serde::{Deserialize, Serialize};

/// The observed live `prereqType` vocabulary (S1 smoke). Only [`PREREQ_TYPE_SET`]
/// creates a bracket edge; everything else is pre-satisfied.
/// The tag without its sponsor/team prefix ("KBN | Crouton" → "Crouton").
/// Display-only — conflict keys and the identity scan keep the full name.
pub fn strip_sponsor(name: &str) -> &str {
    name.rsplit_once(" | ").map_or(name, |(_, tag)| tag)
}

pub const PREREQ_TYPE_SET: &str = "set";
pub const PREREQ_TYPE_SEED: &str = "seed";

/// A set's canonical id string. Preview ids (`preview_<pg>_<round>_<idx>`) and
/// numeric ids share this representation; `scalars::Id` already canonicalizes
/// JSON numbers to strings, matching `prereqId`'s string form.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SetId(pub String);

/// The stable cross-snapshot set key. Set ids swap wholesale from `preview_*`
/// strings to numbers at bracket start, so every persisted or cross-snapshot
/// association keys on `(phase_group, round, identifier)` instead — those
/// survive the swap.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SetKey {
    pub phase_group: String,
    pub round: i32,
    pub identifier: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PlayerId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EntrantId(pub String);

/// Identifies one scheduled bracket (one start.gg event) in config and output.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct BracketId(pub String);

/// A slot's prerequisite: either a real bracket edge to another set, or
/// something the scheduler treats as already satisfied (seed placements,
/// unknown vocabulary, and dangling references to sets the API never returned).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Prereq {
    /// The occupant comes from another set's outcome. `placement` is 1 for the
    /// feeder's winner, 2 for its loser.
    Set { id: SetId, placement: Option<i32> },
    /// Nothing to wait on; the occupant appears (or not) via polling.
    PreSatisfied { raw_type: Option<String> },
}

/// An entrant sitting in a slot. `player_ids` carries every participant's
/// global player id (doubles-safe); it may be empty when the API degraded the
/// identity, in which case conflict tracking falls back to the entrant id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotOccupant {
    pub entrant_id: EntrantId,
    pub display_name: String,
    pub is_disqualified: bool,
    pub player_ids: Vec<PlayerId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Slot {
    pub prereq: Option<Prereq>,
    pub occupant: Option<SlotOccupant>,
}

/// One set as of the latest poll, in scheduler-local form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveSet {
    pub id: SetId,
    pub key: SetKey,
    pub state_int: Option<i32>,
    pub full_round_text: Option<String>,
    /// Unix seconds. Overwritten by every remote action ("latest action
    /// time"), never a stable first-start.
    pub started_at: Option<i64>,
    /// Unix seconds.
    pub completed_at: Option<i64>,
    pub winner_id: Option<EntrantId>,
    pub has_placeholder: bool,
    pub slots: Vec<Slot>,
}

impl LiveSet {
    /// Evidence-based completion: `completedAt` or `winnerId` present.
    pub fn is_completed(&self) -> bool {
        self.completed_at.is_some() || self.winner_id.is_some()
    }

    /// Someone acted on this set remotely and it hasn't finished.
    pub fn is_remotely_active(&self) -> bool {
        self.started_at.is_some() && !self.is_completed()
    }

    /// Whether the state int is a learned/pinned CALLED value. Ints outside
    /// the known set are never treated as busy-evidence.
    pub fn called_evidence(&self, known_called_ints: &[i32]) -> bool {
        self.state_int.is_some_and(|s| known_called_ints.contains(&s))
    }

    pub fn occupants(&self) -> impl Iterator<Item = &SlotOccupant> {
        self.slots.iter().filter_map(|s| s.occupant.as_ref())
    }

    /// Both slots hold a resolved entrant.
    pub fn all_slots_occupied(&self) -> bool {
        !self.slots.is_empty() && self.slots.iter().all(|s| s.occupant.is_some())
    }
}

/// How a phase group schedules its sets; drives the per-group branch in the
/// bracket graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GroupKind {
    /// Single or double elimination: the prereq edges form the real DAG.
    Elimination,
    RoundRobin,
    Swiss {
        num_rounds: i32,
    },
    Unsupported(String),
}

/// Structural facts about one phase group, from the event-structure query.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PhaseGroupInfo {
    pub id: String,
    pub kind: GroupKind,
    /// round number → best-of, when the structure query supplied rounds.
    pub best_of_by_round: BTreeMap<i32, i32>,
    /// Unix seconds; the group's own start time or its wave's.
    pub start_at: Option<i64>,
    pub num_rounds: Option<i32>,
}

/// Non-fatal conversion findings, returned as data (the pure core never logs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelWarning {
    /// An entrant was present but carried no usable id; the slot was degraded
    /// to unoccupied.
    EntrantMissingId { set: SetKey },
    /// An occupant resolved but none of its participants had a player id;
    /// conflict tracking for it falls back to the entrant id.
    IdentityDegraded { set: SetKey, entrant: EntrantId },
    /// A prereq type outside the observed `{"seed", "set"}` vocabulary; the
    /// slot was pre-satisfied like a seed.
    UnknownPrereqType { set: SetKey, raw: String },
    /// A phase group had no usable bracket type or was missing Swiss round
    /// counts; it was mapped to [`GroupKind::Unsupported`].
    UnsupportedGroup { phase_group: String, raw: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    MissingId,
    MissingPhaseGroup,
    MissingRound,
    MissingIdentifier,
}

/// A set the conversion dropped entirely (unusable identity). The poll
/// carries on without it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedSet {
    pub reason: SkipReason,
    pub raw_id: Option<String>,
}

/// Converts one schema set into the local model. Null players degrade the
/// slot, never the poll; only sets with unusable identity are skipped.
pub fn live_set_from_schema(set: get_sets_for_event::Set) -> Result<(LiveSet, Vec<ModelWarning>), SkippedSet> {
    let raw_id = set.id.as_ref().map(|id| id.inner().to_owned());
    let skip = |reason| SkippedSet {
        reason,
        raw_id: raw_id.clone(),
    };

    let id = SetId(raw_id.clone().ok_or_else(|| skip(SkipReason::MissingId))?);
    let phase_group = set
        .phase_group
        .as_ref()
        .and_then(|pg| pg.id.as_ref())
        .map(|pg_id| pg_id.inner().to_owned())
        .ok_or_else(|| skip(SkipReason::MissingPhaseGroup))?;
    let round = set.round.ok_or_else(|| skip(SkipReason::MissingRound))?;
    let identifier = set.identifier.clone().ok_or_else(|| skip(SkipReason::MissingIdentifier))?;
    let key = SetKey {
        phase_group,
        round,
        identifier,
    };

    let mut warnings = Vec::new();
    let slots = set
        .slots
        .into_iter()
        .flatten()
        .flatten()
        .map(|slot| convert_slot(slot, &key, &mut warnings))
        .collect();

    let live = LiveSet {
        id,
        key,
        state_int: set.state,
        full_round_text: set.full_round_text,
        started_at: set.started_at.map(|ts| ts.0),
        completed_at: set.completed_at.map(|ts| ts.0),
        winner_id: set.winner_id.map(|w| EntrantId(w.to_string())),
        has_placeholder: set.has_placeholder.unwrap_or(false),
        slots,
    };
    Ok((live, warnings))
}

fn convert_slot(slot: get_sets_for_event::SetSlot, key: &SetKey, warnings: &mut Vec<ModelWarning>) -> Slot {
    let prereq = match (slot.prereq_type.as_deref(), slot.prereq_id) {
        (Some(PREREQ_TYPE_SET), Some(prereq_id)) => Some(Prereq::Set {
            id: SetId(prereq_id),
            placement: slot.prereq_placement,
        }),
        (Some(ty), _) => {
            if !matches!(ty, PREREQ_TYPE_SEED | PREREQ_TYPE_SET) {
                warnings.push(ModelWarning::UnknownPrereqType {
                    set: key.clone(),
                    raw: ty.to_owned(),
                });
            }
            Some(Prereq::PreSatisfied {
                raw_type: slot.prereq_type.clone(),
            })
        }
        (None, _) => None,
    };
    let occupant = slot.entrant.and_then(|entrant| convert_occupant(entrant, key, warnings));
    Slot { prereq, occupant }
}

fn convert_occupant(entrant: get_sets_for_event::Entrant, key: &SetKey, warnings: &mut Vec<ModelWarning>) -> Option<SlotOccupant> {
    let Some(id) = entrant.id else {
        warnings.push(ModelWarning::EntrantMissingId { set: key.clone() });
        return None;
    };
    let entrant_id = EntrantId(id.inner().to_owned());

    let participants: Vec<_> = entrant.participants.into_iter().flatten().flatten().collect();
    let player_ids: Vec<_> = participants
        .iter()
        .filter_map(|p| p.player.as_ref())
        .filter_map(|player| player.id.as_ref())
        .map(|player_id| PlayerId(player_id.inner().to_owned()))
        .collect();
    if player_ids.is_empty() {
        warnings.push(ModelWarning::IdentityDegraded {
            set: key.clone(),
            entrant: entrant_id.clone(),
        });
    }

    let display_name = entrant
        .name
        .or_else(|| participants.iter().filter_map(|p| p.gamer_tag.clone()).next())
        .unwrap_or_else(|| format!("entrant {}", entrant_id.0));

    Some(SlotOccupant {
        entrant_id,
        display_name,
        is_disqualified: entrant.is_disqualified.unwrap_or(false),
        player_ids,
    })
}

/// Converts every set in a page-worth of nodes, splitting into converted sets,
/// accumulated warnings, and skipped sets.
pub fn live_sets_from_schema(sets: Vec<get_sets_for_event::Set>) -> (Vec<LiveSet>, Vec<ModelWarning>, Vec<SkippedSet>) {
    let mut live = Vec::with_capacity(sets.len());
    let mut warnings = Vec::new();
    let mut skipped = Vec::new();
    for set in sets {
        match live_set_from_schema(set) {
            Ok((set, mut w)) => {
                live.push(set);
                warnings.append(&mut w);
            }
            Err(s) => skipped.push(s),
        }
    }
    (live, warnings, skipped)
}

/// Extracts per-phase-group structure from the event-structure query.
pub fn phase_groups_from_schema(event: &get_event_structure::Event) -> (Vec<PhaseGroupInfo>, Vec<ModelWarning>) {
    let mut infos = Vec::new();
    let mut warnings = Vec::new();
    for pg in event.phase_groups.iter().flatten().flatten() {
        let Some(id) = pg.id.as_ref().map(|id| id.inner().to_owned()) else {
            warnings.push(ModelWarning::UnsupportedGroup {
                phase_group: "<missing id>".to_owned(),
                raw: "phase group without id".to_owned(),
            });
            continue;
        };

        let kind = group_kind(pg, &id, &mut warnings);
        let best_of_by_round = pg
            .rounds
            .iter()
            .flatten()
            .flatten()
            .filter_map(|r| Some((r.number?, r.best_of?)))
            .collect();
        let start_at = pg
            .start_at
            .map(|ts| ts.0)
            .or_else(|| pg.wave.as_ref().and_then(|w| w.start_at.map(|ts| ts.0)));

        infos.push(PhaseGroupInfo {
            id,
            kind,
            best_of_by_round,
            start_at,
            num_rounds: pg.num_rounds,
        });
    }
    (infos, warnings)
}

fn group_kind(pg: &get_event_structure::PhaseGroup, id: &str, warnings: &mut Vec<ModelWarning>) -> GroupKind {
    let mut unsupported = |raw: String| {
        warnings.push(ModelWarning::UnsupportedGroup {
            phase_group: id.to_owned(),
            raw: raw.clone(),
        });
        GroupKind::Unsupported(raw)
    };
    match pg.bracket_type {
        Some(BracketType::SingleElimination | BracketType::DoubleElimination) => GroupKind::Elimination,
        Some(BracketType::RoundRobin) => GroupKind::RoundRobin,
        Some(BracketType::Swiss) => match pg.num_rounds {
            Some(num_rounds) => GroupKind::Swiss { num_rounds },
            None => unsupported("SWISS without numRounds".to_owned()),
        },
        Some(other) => unsupported(format!("{other:?}")),
        None => unsupported("<missing bracketType>".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use bracket_tools_startgg_schema::{
        get_sets_for_event::{Entrant, Participant, PhaseGroup, Player, Set, SetSlot},
        scalars::{Id, Timestamp},
    };

    use super::{live_set_from_schema, LiveSet, ModelWarning, Prereq, SetId, SetKey, SkipReason};

    #[test]
    fn strip_sponsor_takes_the_tag_after_the_last_separator() {
        use super::strip_sponsor;
        assert_eq!(strip_sponsor("KBN | Crouton"), "Crouton");
        assert_eq!(strip_sponsor("Crouton"), "Crouton");
        assert_eq!(strip_sponsor("A | B | Tag"), "Tag");
        assert_eq!(strip_sponsor(""), "");
    }

    fn schema_set() -> Set {
        Set {
            id: Some(Id::new("1001")),
            state: Some(1),
            round: Some(1),
            identifier: Some("A".to_owned()),
            full_round_text: Some("Winners Round 1".to_owned()),
            started_at: None,
            completed_at: None,
            winner_id: None,
            has_placeholder: Some(false),
            phase_group: Some(PhaseGroup { id: Some(Id::new("77")) }),
            slots: Some(vec![
                Some(slot_with_entrant(0, "500", "Alice", Some("42"))),
                Some(slot_with_entrant(1, "501", "Bob", Some("43"))),
            ]),
        }
    }

    fn slot_with_entrant(index: i32, entrant_id: &str, name: &str, player_id: Option<&str>) -> SetSlot {
        SetSlot {
            slot_index: Some(index),
            prereq_id: Some(format!("seed{index}")),
            prereq_type: Some("seed".to_owned()),
            prereq_placement: None,
            entrant: Some(Entrant {
                id: Some(Id::new(entrant_id)),
                name: Some(name.to_owned()),
                is_disqualified: Some(false),
                participants: Some(vec![Some(Participant {
                    gamer_tag: Some(name.to_owned()),
                    player: player_id.map(|id| Player { id: Some(Id::new(id)) }),
                })]),
            }),
        }
    }

    fn converted(set: Set) -> (LiveSet, Vec<ModelWarning>) {
        live_set_from_schema(set).expect("conversion should succeed")
    }

    #[test]
    fn converts_full_set_with_stable_key() {
        let (live, warnings) = converted(schema_set());
        assert_eq!(live.id, SetId("1001".to_owned()));
        assert_eq!(
            live.key,
            SetKey {
                phase_group: "77".to_owned(),
                round: 1,
                identifier: "A".to_owned()
            }
        );
        assert!(live.all_slots_occupied());
        assert_eq!(live.slots[0].occupant.as_ref().unwrap().player_ids[0].0, "42");
        assert!(warnings.is_empty());
    }

    #[test]
    fn set_prereq_keeps_placement() {
        let mut set = schema_set();
        let slot = set.slots.as_mut().unwrap()[0].as_mut().unwrap();
        slot.prereq_type = Some("set".to_owned());
        slot.prereq_id = Some("900".to_owned());
        slot.prereq_placement = Some(2);
        let (live, _) = converted(set);
        assert_eq!(
            live.slots[0].prereq,
            Some(Prereq::Set {
                id: SetId("900".to_owned()),
                placement: Some(2)
            })
        );
    }

    #[test]
    fn seed_and_unknown_prereq_types_pre_satisfy() {
        let mut set = schema_set();
        set.slots.as_mut().unwrap()[1].as_mut().unwrap().prereq_type = Some("bye".to_owned());
        let (live, warnings) = converted(set);
        assert_eq!(
            live.slots[0].prereq,
            Some(Prereq::PreSatisfied {
                raw_type: Some("seed".to_owned())
            })
        );
        assert_eq!(
            live.slots[1].prereq,
            Some(Prereq::PreSatisfied {
                raw_type: Some("bye".to_owned())
            })
        );
        // Only the unknown vocabulary warns; "seed" is known-normal.
        assert_eq!(warnings.len(), 1);
        assert!(matches!(
            &warnings[0],
            ModelWarning::UnknownPrereqType { raw, .. } if raw == "bye"
        ));
    }

    #[test]
    fn null_player_degrades_identity_not_poll() {
        let mut set = schema_set();
        set.slots.as_mut().unwrap()[0]
            .as_mut()
            .unwrap()
            .entrant
            .as_mut()
            .unwrap()
            .participants = Some(vec![Some(Participant {
            gamer_tag: Some("Alice".to_owned()),
            player: None,
        })]);
        let (live, warnings) = converted(set);
        let occupant = live.slots[0].occupant.as_ref().unwrap();
        assert!(occupant.player_ids.is_empty());
        assert!(matches!(warnings[0], ModelWarning::IdentityDegraded { .. }));
    }

    #[test]
    fn entrant_without_id_degrades_slot() {
        let mut set = schema_set();
        set.slots.as_mut().unwrap()[0].as_mut().unwrap().entrant.as_mut().unwrap().id = None;
        let (live, warnings) = converted(set);
        assert!(live.slots[0].occupant.is_none());
        assert!(matches!(warnings[0], ModelWarning::EntrantMissingId { .. }));
    }

    #[test]
    fn missing_identity_fields_skip_the_set() {
        let mut set = schema_set();
        set.identifier = None;
        let err = live_set_from_schema(set).unwrap_err();
        assert_eq!(err.reason, SkipReason::MissingIdentifier);
        assert_eq!(err.raw_id.as_deref(), Some("1001"));
    }

    #[test]
    fn classification_is_evidence_based() {
        let (mut live, _) = converted(schema_set());
        assert!(!live.is_completed());
        assert!(!live.is_remotely_active());

        live.started_at = Some(1_751_000_000);
        assert!(live.is_remotely_active());

        live.winner_id = Some(super::EntrantId("500".to_owned()));
        assert!(live.is_completed());
        assert!(!live.is_remotely_active());

        live.winner_id = None;
        live.completed_at = Some(1_751_000_100);
        assert!(live.is_completed());
    }

    #[test]
    fn called_evidence_only_from_known_ints() {
        let (mut live, _) = converted(schema_set());
        live.state_int = Some(6);
        assert!(live.called_evidence(&[6]));
        assert!(!live.called_evidence(&[]));
        live.state_int = Some(99);
        assert!(!live.called_evidence(&[6]));
    }

    #[test]
    fn preview_and_numeric_ids_yield_identical_keys() {
        let mut preview = schema_set();
        preview.id = Some(Id::new("preview_77_1_0"));
        let (a, _) = converted(preview);
        let (b, _) = converted(schema_set());
        assert_eq!(a.key, b.key);
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn timestamps_convert_to_unix_seconds() {
        let mut set = schema_set();
        set.started_at = Some(Timestamp(1_751_000_000));
        set.completed_at = Some(Timestamp(1_751_000_900));
        set.winner_id = Some(500);
        let (live, _) = converted(set);
        assert_eq!(live.started_at, Some(1_751_000_000));
        assert_eq!(live.completed_at, Some(1_751_000_900));
        assert_eq!(live.winner_id, Some(super::EntrantId("500".to_owned())));
    }
}

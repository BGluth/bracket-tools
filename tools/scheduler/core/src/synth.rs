//! Synthetic bracket builders for deterministic tests, perf runs, and the
//! future `--simulate` driver.
//!
//! Builders emit model-layer [`LiveSet`]s in the shapes S1 observed live:
//! unstarted brackets carry `preview_<pg>_<round>_<idx>` ids, R1 slots hold
//! occupants with seed prereqs, later slots hold set prereqs with
//! winner/loser placements, and bye-degenerate sets are omitted from the set
//! list while other sets' prereqs still reference them (permanent dangling
//! edges).
//!
//! One deliberate deviation from live: when a *losers-side* walkover set is
//! omitted, the downstream slot's prereq is rewired to the walkover's real
//! source (e.g. "loser of the surviving W1 set") instead of left dangling.
//! Live start.gg fills such slots server-side between polls, which a pure
//! simulation can't reproduce; the rewrite keeps winner propagation working.
//! Winners-side byes keep their dangling prereq (the occupant is statically
//! known and pre-placed, as live does).

use std::collections::HashMap;

use crate::model::{
    BracketId, EntrantId, GroupKind, LiveSet, PhaseGroupInfo, PlayerId, Prereq, SetId, SetKey, Slot, SlotOccupant, PREREQ_TYPE_SEED,
};

const WINNER: i32 = 1;
const LOSER: i32 = 2;

/// Fictional gamer tags for `--synth` rehearsal worlds (some carry made-up
/// sponsor prefixes so label widths look like a real bracket). Unit tests
/// keep the deterministic `Player N` vocabulary from [`default_players`].
const SYNTH_TAGS: &[&str] = &[
    "Quasar",
    "Drift",
    "KBN | Nimbus",
    "Wisp",
    "Ember",
    "Talon",
    "Pixel",
    "HXD | Vortex",
    "Karma",
    "Slate",
    "Comet",
    "Rune",
    "ZBT | Havoc",
    "Lotus",
    "Fizz",
    "Onyx",
    "Sable",
    "MNT | Prism",
    "Echo",
    "Rascal",
    "Bramble",
    "Zephyr",
    "K7 | Ivory",
    "Grim",
    "Pesto",
    "Waffle",
    "Noodle",
    "KBN | Crouton",
    "Biscuit",
    "Squid",
    "Tofu",
    "Marble",
    "HXD | Clover",
    "Sprig",
    "Doodle",
    "Fjord",
    "Kelp",
    "ZBT | Moth",
    "Pigeon",
    "Yeti",
    "Goblin",
    "Turnip",
    "MNT | Parry",
    "Ledge",
    "Waveland",
    "Pivot",
    "Crossup",
    "K7 | Meteor",
    "Tumble",
    "Skewer",
    "Gale",
    "Frost",
    "KBN | Cinder",
    "Thistle",
    "Badger",
    "Otter",
    "Lynx",
    "HXD | Heron",
    "Viper",
    "Mantis",
    "Wren",
    "Corvid",
    "ZBT | Dingo",
    "Gecko",
    "Koi",
    "Tapir",
    "Newt",
    "MNT | Osprey",
    "Puffin",
    "Stoat",
    "Vole",
    "Shrike",
    "K7 | Fathom",
    "Umbra",
    "Zenith",
    "Solstice",
    "Cascade",
    "KBN | Latch",
    "Mortar",
    "Anchor",
    "Sprocket",
    "Juniper",
    "HXD | Static",
    "Mango2King",
    "Tempo",
    "Ronin",
    "Dusk",
    "ZBT | Gizmo",
    "Sprout",
    "Jinx",
    "Ferrous",
    "Halcyon",
    "MNT | Bandit",
    "Cobalt",
    "Wick",
    "Tundra",
];

#[derive(Debug, Clone)]
pub struct SynthPlayer {
    pub player_id: String,
    pub name: String,
}

/// One built phase group: its sets plus the structure info the graph builder
/// wants alongside them.
#[derive(Debug, Clone)]
pub struct SynthBracket {
    pub sets: Vec<LiveSet>,
    pub info: PhaseGroupInfo,
}

/// One built event (bracket in scheduler terms), possibly spanning multiple
/// phase groups (pools → DE, swiss → top cut).
#[derive(Debug, Clone)]
pub struct SynthEvent {
    pub id: BracketId,
    pub sets: Vec<LiveSet>,
    pub groups: Vec<PhaseGroupInfo>,
}

/// Players `P1..Pn`, seeded in order.
pub fn default_players(n: usize) -> Vec<SynthPlayer> {
    (1..=n)
        .map(|i| SynthPlayer {
            player_id: format!("P{i}"),
            name: format!("Player {i}"),
        })
        .collect()
}

/// Players `P1..Pn` wearing fictional gamer tags, seeded in order. Pools
/// deeper than the tag list wrap with a numeric suffix (`Quasar 2`).
pub fn tagged_players(n: usize) -> Vec<SynthPlayer> {
    (1..=n)
        .map(|i| {
            let tag = SYNTH_TAGS[(i - 1) % SYNTH_TAGS.len()];
            let name = match (i - 1) / SYNTH_TAGS.len() {
                0 => tag.to_owned(),
                wrap => format!("{tag} {}", wrap + 1),
            };
            SynthPlayer {
                player_id: format!("P{i}"),
                name,
            }
        })
        .collect()
}

/// A double-elimination bracket for `n_entrants` default players, including
/// losers bracket, grand final, and grand-final reset.
pub fn make_de_bracket(pg: u64, n_entrants: usize) -> SynthBracket {
    make_de_bracket_with(pg, &default_players(n_entrants))
}

pub fn make_de_bracket_with(pg: u64, players: &[SynthPlayer]) -> SynthBracket {
    build_elimination(pg, R1Fill::Seeded(players), true)
}

pub fn make_se_bracket(pg: u64, n_entrants: usize) -> SynthBracket {
    build_elimination(pg, R1Fill::Seeded(&default_players(n_entrants)), false)
}

pub fn make_se_bracket_with(pg: u64, players: &[SynthPlayer]) -> SynthBracket {
    build_elimination(pg, R1Fill::Seeded(players), false)
}

/// A single-elimination skeleton whose R1 slots are empty seed-prereq slots —
/// the shape of a top cut waiting on a feeder group's standings.
pub fn make_unseeded_se(pg: u64, n_slots: usize) -> SynthBracket {
    build_elimination(pg, R1Fill::Pending(n_slots), false)
}

/// A round-robin pool: every pair meets exactly once, scheduled into rounds
/// by the circle method; all slots occupied from the start with seed prereqs.
pub fn make_rr_pool(pg: u64, n_entrants: usize) -> SynthBracket {
    make_rr_pool_with(pg, &default_players(n_entrants))
}

pub fn make_rr_pool_with(pg: u64, players: &[SynthPlayer]) -> SynthBracket {
    let n = players.len();
    assert!(n >= 2, "a pool needs at least 2 players");

    let mut ring: Vec<Option<usize>> = (1..=n).map(Some).collect();
    if n % 2 == 1 {
        ring.push(None);
    }
    let ring_len = ring.len();
    let rounds = ring_len - 1;

    let mut sets = Vec::new();
    let mut created = 0usize;
    for round in 1..=rounds {
        let mut idx = 0;
        for i in 0..ring_len / 2 {
            let (Some(a), Some(b)) = (ring[i], ring[ring_len - 1 - i]) else {
                continue;
            };
            sets.push(finished_set(
                pg,
                round as i32,
                idx,
                letters(created),
                format!("Round {round}"),
                [seeded_slot(pg, players, a), seeded_slot(pg, players, b)],
            ));
            created += 1;
            idx += 1;
        }
        let last = ring.pop().expect("ring is non-empty");
        ring.insert(1, last);
    }

    SynthBracket {
        sets,
        info: PhaseGroupInfo {
            id: pg.to_string(),
            kind: GroupKind::RoundRobin,
            best_of_by_round: Default::default(),
            start_at: None,
            num_rounds: Some(rounds as i32),
            phase_id: None,
            phase_order: None,
        },
    }
}

/// A swiss group as live data shows it: only the current (first) round's sets
/// exist; future rounds are synthesized downstream from `num_rounds`.
pub fn make_swiss(pg: u64, n_entrants: usize, num_rounds: i32) -> SynthBracket {
    make_swiss_with(pg, &default_players(n_entrants), num_rounds)
}

pub fn make_swiss_with(pg: u64, players: &[SynthPlayer], num_rounds: i32) -> SynthBracket {
    let sets = (0..players.len() / 2)
        .map(|i| {
            finished_set(
                pg,
                1,
                i,
                letters(i),
                "Round 1".to_owned(),
                [seeded_slot(pg, players, 2 * i + 1), seeded_slot(pg, players, 2 * i + 2)],
            )
        })
        .collect();

    SynthBracket {
        sets,
        info: PhaseGroupInfo {
            id: pg.to_string(),
            kind: GroupKind::Swiss { num_rounds },
            best_of_by_round: Default::default(),
            start_at: None,
            num_rounds: Some(num_rounds),
            phase_id: None,
            phase_order: None,
        },
    }
}

/// A 7-event world shaped like FBR 100: six DE singles events of varying
/// size plus a swiss → top-cut event, over a shared player pool with heavy
/// cross-event overlap (ironman material).
pub fn make_fbr_world() -> Vec<SynthEvent> {
    let pool = default_players(96);
    let de_event = |slug: &str, pg: u64, players: &[SynthPlayer]| {
        let bracket = make_de_bracket_with(pg, players);
        SynthEvent {
            id: BracketId(format!("synth/{slug}")),
            sets: bracket.sets,
            groups: vec![bracket.info],
        }
    };
    let melee_players: Vec<_> = pool[0..16].iter().chain(&pool[64..80]).cloned().collect();

    let pokemon_swiss = make_swiss_with(1007, &pool[52..61], 4);
    let pokemon_cut = make_unseeded_se(1008, 4);
    let pokemon = SynthEvent {
        id: BracketId("synth/pokemon-champions".to_owned()),
        sets: pokemon_swiss.sets.into_iter().chain(pokemon_cut.sets).collect(),
        groups: vec![pokemon_swiss.info, pokemon_cut.info],
    };

    vec![
        de_event("ultimate-singles", 1001, &pool[0..64]),
        de_event("melee-singles", 1002, &melee_players),
        de_event("rivals-2-singles", 1003, &pool[16..32]),
        de_event("brawl-singles", 1004, &pool[32..44]),
        de_event("mugen-singles", 1005, &pool[44..52]),
        de_event("special-smash", 1006, &pool[0..24]),
        pokemon,
    ]
}

/// Rewrites every set id to a numeric one (as bracket start does live),
/// rewriting resolvable prereq edges along the way. References to omitted
/// (bye-degenerate) sets stay dangling, as observed live.
pub fn materialize_ids(sets: &[LiveSet], first_numeric_id: u64) -> Vec<LiveSet> {
    let mapping: HashMap<&str, String> = sets
        .iter()
        .enumerate()
        .map(|(i, set)| (set.id.0.as_str(), (first_numeric_id + i as u64).to_string()))
        .collect();

    sets.iter()
        .cloned()
        .map(|mut set| {
            set.id = SetId(mapping[set.id.0.as_str()].clone());
            for slot in &mut set.slots {
                if let Some(Prereq::Set { id, .. }) = &mut slot.prereq {
                    if let Some(numeric) = mapping.get(id.0.as_str()) {
                        *id = SetId(numeric.clone());
                    }
                }
            }
            set
        })
        .collect()
}

/// Marks a set completed with the given slot's occupant as winner.
pub fn complete(set: &mut LiveSet, winner_slot: usize, at: i64) {
    let winner = set.slots[winner_slot]
        .occupant
        .as_ref()
        .expect("winner slot must be occupied")
        .entrant_id
        .clone();
    set.winner_id = Some(winner);
    set.completed_at = Some(at);
    set.state_int = Some(3);
}

enum R1Fill<'a> {
    Seeded(&'a [SynthPlayer]),
    Pending(usize),
}

/// Where a proto slot's occupant comes from during construction. `Vacant`
/// means nobody can ever arrive (a bye); `Pending` means the server will fill
/// it later (cross-group progression), which keeps the set alive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Entrant(usize),
    FromSet { set: usize, placement: i32 },
    Pending,
    Vacant,
}

struct ProtoSlot {
    source: Source,
    /// The as-built edge, before walkover rewiring — emitted as a dangling
    /// prereq when the occupant was statically pre-placed.
    original: Option<(usize, i32)>,
}

struct ProtoSet {
    round: i32,
    idx: usize,
    identifier: String,
    slots: Vec<ProtoSlot>,
}

fn build_elimination(pg: u64, fill: R1Fill<'_>, double: bool) -> SynthBracket {
    let (n, players) = match fill {
        R1Fill::Seeded(players) => (players.len(), Some(players)),
        R1Fill::Pending(n) => (n, None),
    };
    assert!(n >= 2, "a bracket needs at least 2 entrants");
    assert!(!double || n >= 3, "a DE bracket needs at least 3 entrants");
    let k = usize::BITS - (n - 1).leading_zeros();
    let bracket_size = 1usize << k;
    let k = k as usize;

    let mut sets: Vec<ProtoSet> = Vec::new();
    let order = seeding_order(k);

    let mut winners: Vec<Vec<usize>> = Vec::new();
    let r1_slot = |seed: usize| ProtoSlot {
        source: match players {
            Some(_) if seed <= n => Source::Entrant(seed),
            Some(_) => Source::Vacant,
            None => Source::Pending,
        },
        original: None,
    };
    let round1 = (0..bracket_size / 2)
        .map(|i| push_set(&mut sets, 1, i, [r1_slot(order[2 * i]), r1_slot(order[2 * i + 1])]))
        .collect();
    winners.push(round1);
    for r in 2..=k {
        let prev = winners[r - 2].clone();
        let round = (0..prev.len() / 2)
            .map(|i| push_set(&mut sets, r as i32, i, [from(prev[2 * i], WINNER), from(prev[2 * i + 1], WINNER)]))
            .collect();
        winners.push(round);
    }

    if double {
        let mut losers: Vec<Vec<usize>> = Vec::new();
        let w1 = &winners[0];
        let l1 = (0..w1.len() / 2)
            .map(|i| push_set(&mut sets, -1, i, [from(w1[2 * i], LOSER), from(w1[2 * i + 1], LOSER)]))
            .collect();
        losers.push(l1);
        for j in 1..=(k - 1) {
            let dropdowns = winners[j].clone();
            let prev = losers[2 * j - 2].clone();
            let major = (0..dropdowns.len())
                .map(|i| push_set(&mut sets, -(2 * j as i32), i, [from(dropdowns[i], LOSER), from(prev[i], WINNER)]))
                .collect::<Vec<_>>();
            losers.push(major.clone());
            if j <= k - 2 {
                let minor = (0..major.len() / 2)
                    .map(|i| {
                        push_set(
                            &mut sets,
                            -(2 * j as i32 + 1),
                            i,
                            [from(major[2 * i], WINNER), from(major[2 * i + 1], WINNER)],
                        )
                    })
                    .collect();
                losers.push(minor);
            }
        }

        let winners_final = winners[k - 1][0];
        let losers_final = losers.last().expect("k >= 2 gives at least one losers round")[0];
        let gf = push_set(
            &mut sets,
            (k + 1) as i32,
            0,
            [from(winners_final, WINNER), from(losers_final, WINNER)],
        );
        push_set(&mut sets, (k + 2) as i32, 0, [from(gf, WINNER), from(gf, LOSER)]);
    }

    let omitted = collapse(&mut sets);
    emit(pg, &sets, &omitted, players, k, double)
}

fn push_set(sets: &mut Vec<ProtoSet>, round: i32, idx: usize, slots: [ProtoSlot; 2]) -> usize {
    let identifier = letters(sets.len());
    sets.push(ProtoSet {
        round,
        idx,
        identifier,
        slots: slots.into(),
    });
    sets.len() - 1
}

fn from(set: usize, placement: i32) -> ProtoSlot {
    ProtoSlot {
        source: Source::FromSet { set, placement },
        original: Some((set, placement)),
    }
}

/// Omits walkover/dead sets to fixpoint, mirroring live behavior. A set with
/// one live source forwards that source to whoever wanted its winner (its
/// "loser" becomes nobody); a set with no live sources vacates both outputs.
fn collapse(sets: &mut [ProtoSet]) -> Vec<bool> {
    let mut omitted = vec![false; sets.len()];
    loop {
        let mut changed = false;
        for i in 0..sets.len() {
            if omitted[i] {
                continue;
            }
            let live: Vec<usize> = (0..sets[i].slots.len())
                .filter(|&s| is_live(sets[i].slots[s].source, &omitted))
                .collect();
            if live.len() == sets[i].slots.len() {
                continue;
            }
            let replacement = live.first().map(|&s| sets[i].slots[s].source);
            omitted[i] = true;
            changed = true;

            for set_proto in sets.iter_mut() {
                for slot in &mut set_proto.slots {
                    let Source::FromSet { set, placement } = slot.source else {
                        continue;
                    };
                    if set != i {
                        continue;
                    }
                    slot.source = match (placement, replacement) {
                        (WINNER, Some(source)) => source,
                        _ => Source::Vacant,
                    };
                }
            }
        }
        if !changed {
            return omitted;
        }
    }
}

fn is_live(source: Source, omitted: &[bool]) -> bool {
    match source {
        Source::Entrant(_) | Source::Pending => true,
        Source::FromSet { set, .. } => !omitted[set],
        Source::Vacant => false,
    }
}

fn emit(pg: u64, sets: &[ProtoSet], omitted: &[bool], players: Option<&[SynthPlayer]>, k: usize, double: bool) -> SynthBracket {
    let live_sets = sets
        .iter()
        .enumerate()
        .filter(|(i, _)| !omitted[*i])
        .map(|(_, proto)| {
            let slots = proto
                .slots
                .iter()
                .map(|slot| match slot.source {
                    Source::Entrant(seed) => Slot {
                        prereq: match slot.original {
                            Some((target, placement)) => Some(Prereq::Set {
                                id: SetId(preview_id(pg, &sets[target])),
                                placement: Some(placement),
                            }),
                            None => Some(seed_prereq()),
                        },
                        occupant: Some(occupant(pg, players.expect("entrant sources only occur when seeded"), seed)),
                    },
                    Source::FromSet { set, placement } => Slot {
                        prereq: Some(Prereq::Set {
                            id: SetId(preview_id(pg, &sets[set])),
                            placement: Some(placement),
                        }),
                        occupant: None,
                    },
                    Source::Pending => Slot {
                        prereq: Some(seed_prereq()),
                        occupant: None,
                    },
                    Source::Vacant => unreachable!("collapse omits every set with a vacant slot"),
                })
                .collect();

            LiveSet {
                id: SetId(preview_id(pg, proto)),
                key: SetKey {
                    phase_group: pg.to_string(),
                    round: proto.round,
                    identifier: proto.identifier.clone(),
                },
                state_int: Some(1),
                full_round_text: Some(round_name(proto.round, k, double)),
                started_at: None,
                completed_at: None,
                winner_id: None,
                has_placeholder: false,
                slots,
            }
        })
        .collect();

    SynthBracket {
        sets: live_sets,
        info: PhaseGroupInfo {
            id: pg.to_string(),
            kind: GroupKind::Elimination,
            best_of_by_round: Default::default(),
            start_at: None,
            num_rounds: None,
            phase_id: None,
            phase_order: None,
        },
    }
}

fn finished_set(pg: u64, round: i32, idx: usize, identifier: String, round_text: String, slots: [Slot; 2]) -> LiveSet {
    LiveSet {
        id: SetId(format!("preview_{pg}_{round}_{idx}")),
        key: SetKey {
            phase_group: pg.to_string(),
            round,
            identifier,
        },
        state_int: Some(1),
        full_round_text: Some(round_text),
        started_at: None,
        completed_at: None,
        winner_id: None,
        has_placeholder: false,
        slots: slots.into(),
    }
}

fn preview_id(pg: u64, proto: &ProtoSet) -> String {
    format!("preview_{pg}_{}_{}", proto.round, proto.idx)
}

fn seed_prereq() -> Prereq {
    Prereq::PreSatisfied {
        raw_type: Some(PREREQ_TYPE_SEED.to_owned()),
    }
}

fn seeded_slot(pg: u64, players: &[SynthPlayer], seed: usize) -> Slot {
    Slot {
        prereq: Some(seed_prereq()),
        occupant: Some(occupant(pg, players, seed)),
    }
}

fn occupant(pg: u64, players: &[SynthPlayer], seed: usize) -> SlotOccupant {
    let player = &players[seed - 1];
    SlotOccupant {
        entrant_id: EntrantId(format!("{pg}-e{seed}")),
        display_name: player.name.clone(),
        is_disqualified: false,
        player_ids: vec![PlayerId(player.player_id.clone())],
    }
}

/// 1-based seeds in bracket-position order: adjacent pairs are R1 sets, and
/// the top seed's opponent is always the lowest seed in scope.
fn seeding_order(k: usize) -> Vec<usize> {
    let mut order = vec![1];
    for round in 1..=k {
        let size = 1 << round;
        order = order.iter().flat_map(|&s| [s, size + 1 - s]).collect();
    }
    order
}

fn round_name(round: i32, k: usize, double: bool) -> String {
    let k = k as i32;
    if !double {
        return if round == k { "Final".to_owned() } else { format!("Round {round}") };
    }
    match round {
        r if r == k + 2 => "Grand Final Reset".to_owned(),
        r if r == k + 1 => "Grand Final".to_owned(),
        r if r == k => "Winners Final".to_owned(),
        r if r > 0 => format!("Winners Round {r}"),
        r if -r == 2 * k - 2 => "Losers Final".to_owned(),
        r => format!("Losers Round {}", -r),
    }
}

/// 0 → "A", 25 → "Z", 26 → "AA" (bijective base-26, like start.gg identifiers).
fn letters(mut i: usize) -> String {
    let mut out = Vec::new();
    loop {
        out.push(b'A' + (i % 26) as u8);
        i /= 26;
        if i == 0 {
            break;
        }
        i -= 1;
    }
    out.reverse();
    String::from_utf8(out).expect("letters are ascii")
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{letters, make_de_bracket, make_fbr_world, make_rr_pool, make_swiss, make_unseeded_se, materialize_ids, seeding_order};
    use crate::model::{GroupKind, Prereq, SetId};

    #[test]
    fn seeding_order_matches_standard_bracket() {
        assert_eq!(seeding_order(2), vec![1, 4, 2, 3]);
        assert_eq!(seeding_order(3), vec![1, 8, 4, 5, 2, 7, 3, 6]);
    }

    #[test]
    fn letters_are_bijective_base_26() {
        assert_eq!(letters(0), "A");
        assert_eq!(letters(25), "Z");
        assert_eq!(letters(26), "AA");
        assert_eq!(letters(27), "AB");
    }

    #[test]
    fn de_4_has_the_classic_seven_sets() {
        let bracket = make_de_bracket(9, 4);
        assert_eq!(bracket.sets.len(), 7);

        let by_round: Vec<i32> = bracket.sets.iter().map(|s| s.key.round).collect();
        assert_eq!(by_round, vec![1, 1, 2, -1, -2, 3, 4]);

        let gf = &bracket.sets[5];
        assert_eq!(
            gf.slots[0].prereq,
            Some(Prereq::Set {
                id: SetId("preview_9_2_0".to_owned()),
                placement: Some(1)
            })
        );
        assert_eq!(
            gf.slots[1].prereq,
            Some(Prereq::Set {
                id: SetId("preview_9_-2_0".to_owned()),
                placement: Some(1)
            })
        );

        let reset = &bracket.sets[6];
        for (slot, placement) in reset.slots.iter().zip([1, 2]) {
            assert_eq!(
                slot.prereq,
                Some(Prereq::Set {
                    id: SetId("preview_9_3_0".to_owned()),
                    placement: Some(placement)
                })
            );
        }
    }

    #[test]
    fn de_3_collapses_the_bye_like_live() {
        let bracket = make_de_bracket(9, 3);
        assert_eq!(bracket.sets.len(), 5);

        // W2: seed 1's bye pre-places the occupant but keeps the dangling
        // prereq to the omitted W1 set.
        let w2 = bracket.sets.iter().find(|s| s.key.round == 2).unwrap();
        assert!(w2.slots[0].occupant.is_some());
        assert_eq!(
            w2.slots[0].prereq,
            Some(Prereq::Set {
                id: SetId("preview_9_1_0".to_owned()),
                placement: Some(1)
            })
        );

        // L2 slot 1 was rewired from the omitted L1 walkover to "loser of the
        // real W1 set".
        let l2 = bracket.sets.iter().find(|s| s.key.round == -2).unwrap();
        assert_eq!(
            l2.slots[1].prereq,
            Some(Prereq::Set {
                id: SetId("preview_9_1_1".to_owned()),
                placement: Some(2)
            })
        );
    }

    #[test]
    fn de_57_has_standard_set_count_and_seven_danglings() {
        let bracket = make_de_bracket(9, 57);
        assert_eq!(bracket.sets.len(), 2 * 57 - 1);

        let live_ids: HashSet<&str> = bracket.sets.iter().map(|s| s.id.0.as_str()).collect();
        let dangling: Vec<_> = bracket
            .sets
            .iter()
            .flat_map(|s| &s.slots)
            .filter_map(|slot| match &slot.prereq {
                Some(Prereq::Set { id, .. }) if !live_ids.contains(id.0.as_str()) => Some(id),
                _ => None,
            })
            .collect();
        assert_eq!(dangling.len(), 7, "one dangling edge per bye");

        // Every surviving slot is either occupied or fed by a live set.
        for set in &bracket.sets {
            for slot in &set.slots {
                let fed = matches!(&slot.prereq, Some(Prereq::Set { id, .. }) if live_ids.contains(id.0.as_str()));
                assert!(slot.occupant.is_some() || fed, "starved slot in {:?}", set.key);
            }
        }
    }

    #[test]
    fn rr_pools_pair_everyone_exactly_once() {
        for n in [4, 5] {
            let bracket = make_rr_pool(9, n);
            assert_eq!(bracket.sets.len(), n * (n - 1) / 2);

            let mut pairs = HashSet::new();
            for set in &bracket.sets {
                assert!(set.all_slots_occupied());
                let mut ids: Vec<_> = set.occupants().map(|o| o.entrant_id.0.clone()).collect();
                ids.sort();
                assert!(pairs.insert(ids), "duplicate pairing in {n}-player pool");
            }
        }
    }

    #[test]
    fn swiss_builds_only_round_one() {
        let bracket = make_swiss(9, 9, 4);
        assert_eq!(bracket.sets.len(), 4);
        assert!(bracket.sets.iter().all(|s| s.key.round == 1 && s.all_slots_occupied()));
        assert_eq!(bracket.info.kind, GroupKind::Swiss { num_rounds: 4 });
    }

    #[test]
    fn unseeded_se_keeps_pending_slots_alive() {
        let bracket = make_unseeded_se(9, 4);
        assert_eq!(bracket.sets.len(), 3);
        let r1: Vec<_> = bracket.sets.iter().filter(|s| s.key.round == 1).collect();
        assert_eq!(r1.len(), 2);
        assert!(r1.iter().all(|s| s.slots.iter().all(|slot| slot.occupant.is_none())));
    }

    #[test]
    fn materialize_swaps_ids_but_not_keys() {
        let bracket = make_de_bracket(9, 8);
        let numeric = materialize_ids(&bracket.sets, 5000);

        for (before, after) in bracket.sets.iter().zip(&numeric) {
            assert_eq!(before.key, after.key);
            after.id.0.parse::<u64>().expect("numeric id");
        }
        // No byes in a full 8 bracket: every edge rewrote to a numeric id.
        for set in &numeric {
            for slot in &set.slots {
                if let Some(Prereq::Set { id, .. }) = &slot.prereq {
                    id.0.parse::<u64>().expect("rewritten edge");
                }
            }
        }
    }

    #[test]
    fn fbr_world_is_seven_events_with_overlap() {
        let world = make_fbr_world();
        assert_eq!(world.len(), 7);

        let pokemon = world.last().unwrap();
        assert_eq!(pokemon.groups.len(), 2);

        let total_sets: usize = world.iter().map(|e| e.sets.len()).sum();
        assert!(total_sets > 250, "got {total_sets}");

        // P1 irons through ultimate, melee, and special smash.
        let events_with_p1 = world
            .iter()
            .filter(|e| {
                e.sets
                    .iter()
                    .flat_map(|s| s.occupants())
                    .any(|o| o.player_ids.iter().any(|p| p.0 == "P1"))
            })
            .count();
        assert_eq!(events_with_p1, 3);
    }
}

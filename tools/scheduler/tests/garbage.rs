//! Garbage-tolerance: malformed and adversarial snapshots must degrade into
//! warnings/skips, never panics, all the way through convert → graph →
//! conflict → recompute → render.

use bracket_tools_scheduler::{
    app::{update, AppState, BracketBootstrap, Msg, PollOutcome, PollResult},
    config::{BracketConfig, BracketMode, SchedulerConfig, SetupId},
    model::{live_sets_from_schema, BracketId, LiveSet, Prereq, SetId, Slot},
    synth::{make_de_bracket, SynthBracket},
    ui,
};
use bracket_tools_startgg_schema::{
    get_sets_for_event::{Entrant, Participant, PhaseGroup, Player, Set, SetSlot},
    scalars::Id,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{backend::TestBackend, Terminal};

const NOW: i64 = 1_751_000_000_000;

fn schema_set(id: &str, round: Option<i32>, identifier: Option<&str>, pg: Option<&str>) -> Set {
    Set {
        id: Some(Id::new(id)),
        state: Some(-3),
        round,
        identifier: identifier.map(str::to_owned),
        full_round_text: None,
        started_at: None,
        completed_at: None,
        winner_id: Some(-42),
        has_placeholder: Some(true),
        phase_group: pg.map(|p| PhaseGroup { id: Some(Id::new(p)) }),
        slots: Some(vec![
            None,
            Some(SetSlot {
                slot_index: None,
                prereq_id: Some("preview_missing_9_9".to_owned()),
                prereq_type: Some("teleport".to_owned()),
                prereq_placement: Some(7),
                entrant: Some(Entrant {
                    id: None,
                    name: None,
                    is_disqualified: None,
                    participants: Some(vec![
                        None,
                        Some(Participant {
                            gamer_tag: None,
                            player: Some(Player { id: None }),
                        }),
                    ]),
                }),
            }),
        ]),
    }
}

fn app_with(sets: Vec<LiveSet>, bracket: &SynthBracket) -> AppState {
    let config = SchedulerConfig {
        setups: vec![SetupId(1), SetupId(2)],
        brackets: vec![BracketConfig {
            pool: vec![SetupId(1), SetupId(2)],
            ..BracketConfig::new("garbage")
        }],
        ..SchedulerConfig::default()
    };
    let boots = vec![BracketBootstrap {
        id: BracketId("garbage".to_owned()),
        sets,
        groups: vec![bracket.info.clone()],
        mode: BracketMode::Full,
        start_at: None,
        pool: vec![SetupId(1), SetupId(2)],
        duration_prior_secs: 480,
        prior_weight: 4.0,
        characters: Vec::new(),
    }];
    AppState::new(config, false, boots, NOW)
}

fn render(state: &AppState) {
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal.draw(|frame| ui::draw(frame, state, NOW)).unwrap();
}

#[test]
fn malformed_schema_sets_convert_to_warnings_and_skips_not_panics() {
    let garbage = vec![
        schema_set("g1", Some(1), Some("A"), Some("77")),
        schema_set("g2", None, Some("B"), Some("77")), // missing round: skipped
        schema_set("g3", Some(1), None, Some("77")),   // missing identifier: skipped
        schema_set("g4", Some(1), Some("C"), None),    // missing phase group: skipped
        Set {
            id: None, // missing id: skipped
            state: None,
            round: None,
            identifier: None,
            full_round_text: None,
            started_at: None,
            completed_at: None,
            winner_id: None,
            has_placeholder: None,
            phase_group: None,
            slots: None,
        },
    ];
    let (sets, warnings, skipped) = live_sets_from_schema(garbage);
    assert_eq!(sets.len(), 1);
    assert_eq!(skipped.len(), 4);
    assert!(!warnings.is_empty(), "degraded identity + unknown prereq type warn");
}

#[test]
fn adversarial_snapshot_survives_the_full_pipeline() {
    let bracket = make_de_bracket(77, 8);
    // Start healthy, then poll in a snapshot that is pure nonsense: cyclic
    // prereqs, duplicate keys, self-references, dangling edges, empty slots.
    let mut state = app_with(bracket.sets.clone(), &bracket);

    let (mut garbage, _, _) = live_sets_from_schema(vec![
        schema_set("g1", Some(1), Some("A"), Some("77")),
        schema_set("g2", Some(1), Some("A"), Some("77")),  // duplicate SetKey
        schema_set("g3", Some(-5), Some("Z"), Some("77")), // negative round
    ]);
    // A two-set prereq cycle plus a self-referential set.
    garbage.push(LiveSet {
        id: SetId("c1".to_owned()),
        key: bracket.sets[0].key.clone(),
        state_int: None,
        full_round_text: None,
        started_at: None,
        completed_at: None,
        winner_id: None,
        has_placeholder: false,
        slots: vec![
            Slot {
                prereq: Some(Prereq::Set {
                    id: SetId("c2".to_owned()),
                    placement: Some(1),
                }),
                occupant: None,
            },
            Slot {
                prereq: Some(Prereq::Set {
                    id: SetId("c1".to_owned()),
                    placement: Some(2),
                }),
                occupant: None,
            },
        ],
    });
    garbage.push(LiveSet {
        id: SetId("c2".to_owned()),
        key: bracket.sets[1].key.clone(),
        state_int: None,
        full_round_text: None,
        started_at: None,
        completed_at: None,
        winner_id: None,
        has_placeholder: false,
        slots: vec![Slot {
            prereq: Some(Prereq::Set {
                id: SetId("c1".to_owned()),
                placement: Some(1),
            }),
            occupant: None,
        }],
    });

    update(
        &mut state,
        Msg::Poll(PollResult {
            bracket: BracketId("garbage".to_owned()),
            seq: 1,
            captured_at: NOW,
            outcome: PollOutcome::Snapshot {
                sets: garbage,
                warnings: Vec::new(),
                skipped: Vec::new(),
            },
        }),
        NOW + 1000,
    );

    // Keys, picker, ticks, and rendering must all survive the nonsense.
    render(&state);
    for key in ['1', 'z', 'u', 'p', 'f', 'r'] {
        update(
            &mut state,
            Msg::Key(KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE)),
            NOW + 2000,
        );
    }
    update(&mut state, Msg::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), NOW + 2000);
    update(&mut state, Msg::Tick, NOW + 3000);
    render(&state);
}

#[test]
fn empty_snapshot_after_data_is_tolerated() {
    let bracket = make_de_bracket(77, 8);
    let mut state = app_with(bracket.sets.clone(), &bracket);
    let empty_snapshot = |seq: u64| {
        Msg::Poll(PollResult {
            bracket: BracketId("garbage".to_owned()),
            seq,
            captured_at: NOW + seq as i64 * 1000,
            outcome: PollOutcome::Snapshot {
                sets: Vec::new(),
                warnings: Vec::new(),
                skipped: Vec::new(),
            },
        })
    };

    // One empty snapshot: the tearing guard retains every set for a grace
    // cycle, so nothing vanishes off the queue yet.
    update(&mut state, empty_snapshot(1), NOW + 1000);
    assert!(!state.world.queue.is_empty(), "sets retained one cycle");

    // A second consecutive empty snapshot drops them for real.
    update(&mut state, empty_snapshot(2), NOW + 2000);
    assert!(state.world.queue.is_empty());
    render(&state);
}

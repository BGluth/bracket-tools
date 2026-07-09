//! Headless end-to-end over the real S1 capture corpus: capture-backed
//! FixtureSource → preflight → AppState bootstrap → a real poll cycle →
//! update → TestBackend render. The closest thing to launching the TUI that
//! runs without a terminal or network.
//!
//! Env-gated like tests/fixture_replay.rs: skips with a message when the
//! captures aren't mounted.

use std::{env, path::PathBuf, time::Duration};

use bracket_tools_scheduler::{
    app::{update, AppState, Msg, PollFailure},
    config::{BracketConfig, SchedulerConfig, SetupCounts},
    fixture_source::{classify_fixture_error, FixtureSource},
    model::BracketId,
    poller::{poll_cycle, PollerConfig},
    preflight::preflight,
    ui,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{backend::TestBackend, Terminal};

const NOW: i64 = 1_751_000_000_000;

const FBR_EVENTS: [&str; 7] = [
    "tournament/french-bread-rumble-100/event/ultimate-singles",
    "tournament/french-bread-rumble-100/event/melee-singles",
    "tournament/french-bread-rumble-100/event/brawl-singles",
    "tournament/french-bread-rumble-100/event/rivals-2-singles",
    "tournament/french-bread-rumble-100/event/mugen-singles",
    "tournament/french-bread-rumble-100/event/special-smash",
    "tournament/french-bread-rumble-100/event/pokemon-champions-4v4-double-battle",
];

fn captures_dir() -> Option<PathBuf> {
    let dir = match env::var("BRACKET_TOOLS_CAPTURES") {
        Ok(dir) => PathBuf::from(dir),
        Err(_) => PathBuf::from(env::var("HOME").ok()?).join("work/personal/bracket-tools-captures/2026-07-05_s1_smoke"),
    };
    if dir.is_dir() {
        Some(dir)
    } else {
        eprintln!("skipping headless e2e: captures not found at {}", dir.display());
        None
    }
}

fn fbr_config() -> SchedulerConfig {
    SchedulerConfig {
        setups: Some(SetupCounts::Uniform(8)),
        brackets: FBR_EVENTS.iter().map(|slug| BracketConfig::new(*slug)).collect(),
        tournament_slug: Some("tournament/french-bread-rumble-100".to_owned()),
        known_called_state_int: Some(6),
        known_in_progress_state_int: Some(2),
        ..SchedulerConfig::default()
    }
}

#[tokio::test]
async fn full_fbr_world_boots_polls_and_renders() {
    let Some(dir) = captures_dir() else {
        return;
    };
    let source = FixtureSource::from_captures(&dir).expect("capture corpus loads");
    let config = fbr_config();

    // Preflight over all 7 real events.
    let report = preflight(&source, &config, Duration::from_secs(5), true, classify_fixture_error).await;
    assert!(report.fatal.is_none(), "{}", report.render());
    assert!(report.writes_armed);
    let rendered = report.render();
    assert!(rendered.contains("french-bread-rumble-100"), "{rendered}");

    // Bootstrap the app. The FBR captures are pre-tournament (preview ids,
    // empty R1 in some events), so the world may or may not have callables —
    // what matters is that the full pipeline holds together.
    let mut state = AppState::new(config, report.writes_armed, report.into_bootstraps(), NOW);
    assert_eq!(state.brackets.len(), 7);
    let total_sets: usize = state.brackets.iter().map(|b| b.state.sets.len()).sum();
    assert!(total_sets > 300, "the corpus carries the full FBR skeleton, got {total_sets}");

    // A real poll cycle over the same source feeds Msg::Poll back in.
    let events: Vec<BracketId> = state.brackets.iter().map(|b| b.state.id.clone()).collect();
    let poller_config = PollerConfig {
        interval: Duration::from_secs(30),
        request_timeout: Duration::from_secs(5),
        concurrency: 3,
    };
    let classify = |e: &_| -> PollFailure { classify_fixture_error(e) };
    let results = poll_cycle(&source, &events, 1, &poller_config, &classify).await;
    assert_eq!(results.len(), 7);
    for result in results {
        update(&mut state, Msg::Poll(result), NOW + 30_000);
    }
    assert!(state.brackets.iter().all(|b| b.applied_seq == 1));

    // Drive some keys and render frames at realistic sizes.
    update(
        &mut state,
        Msg::Key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE)),
        NOW + 31_000,
    );
    update(&mut state, Msg::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)), NOW + 31_000);
    update(&mut state, Msg::Tick, NOW + 32_000);

    for (width, height) in [(80, 24), (130, 40), (200, 60)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| ui::draw(frame, &state, NOW + 32_000)).unwrap();
    }

    // The projection pipeline ran over the whole corpus.
    assert_eq!(state.world.summaries.len(), 7);
    assert!(state.world.overall_projected_finish.is_some());
}

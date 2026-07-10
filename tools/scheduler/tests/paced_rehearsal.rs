//! Env-gated (like fixture_replay.rs): builds the paced rehearsal over the
//! real S1 capture corpus with the shipped example config, then fast-forwards
//! the shared clock and checks the played-out world.

use std::{env, path::PathBuf, time::Duration};

use bracket_tools_scheduler::{
    app::{update, AppState, Msg, PollFailure},
    fixture_source::{classify_fixture_error, FixtureSource},
    model::{live_sets_from_schema, BracketId},
    poller::{poll_cycle, PollerConfig},
    preflight::{preflight, PreflightEnv},
    rehearsal::install_rehearsal,
    set_source::SetSource,
    ui, SchedulerConfig,
};
use ratatui::{backend::TestBackend, Terminal};

const NOW: i64 = 1_751_000_000_000;

fn captures_dir() -> Option<PathBuf> {
    let dir = match env::var("BRACKET_TOOLS_CAPTURES") {
        Ok(dir) => PathBuf::from(dir),
        Err(_) => PathBuf::from(env::var("HOME").ok()?).join("work/personal/bracket-tools-captures/2026-07-05_s1_smoke"),
    };
    dir.is_dir().then_some(dir)
}

fn example_config() -> SchedulerConfig {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/fbr-100.toml");
    SchedulerConfig::load(&path).expect("example config loads")
}

#[tokio::test]
async fn paced_rehearsal_plays_the_fbr_corpus_to_completion() {
    let Some(dir) = captures_dir() else {
        eprintln!("skipping paced-rehearsal test: capture corpus absent");
        return;
    };
    let mut source = FixtureSource::from_captures(&dir).unwrap();
    let config = example_config();

    let report = install_rehearsal(&mut source, &config, 60.0, NOW).await.unwrap();
    assert_eq!(report.frames.len(), 7, "all configured brackets get timelines");
    assert!(report.finishes_at > report.started_at);

    // Fast-forward past the whole script and check every bracket played out.
    let wall = Duration::from_millis((report.finishes_at - report.started_at) as u64 + 1_000);
    source.rewind_clock(wall);
    for bracket in &config.brackets {
        let sets = source.fetch_event_sets(&bracket.slug).await.unwrap();
        let (live, _, skipped) = live_sets_from_schema(sets);
        assert!(skipped.is_empty(), "{}: {skipped:?}", bracket.slug);
        let open: Vec<_> = live.iter().filter(|s| !s.is_completed()).collect();
        for set in &open {
            eprintln!(
                "{}: OPEN {} round {} {:?} slots {:?}",
                bracket.slug,
                set.id.0,
                set.key.round,
                set.full_round_text,
                set.slots
                    .iter()
                    .map(|s| (s.occupant.as_ref().map(|o| o.display_name.as_str()), &s.prereq))
                    .collect::<Vec<_>>(),
            );
        }
        // A DE bracket's un-fired GF reset legitimately stays open; anything
        // more means the script starved.
        assert!(open.len() <= 1, "{}: {} open sets at script end", bracket.slug, open.len());
    }
    assert!(report.blocked.is_empty(), "blocked: {:?}", report.blocked);
}

/// The real app path over a paced rehearsal: preflight boots off frame 0
/// (materialized ids, brackets live now), and successive poll cycles ingest
/// the scripted completions as the clock advances.
#[tokio::test]
async fn app_ingests_a_paced_rehearsal_across_poll_cycles() {
    let Some(dir) = captures_dir() else {
        eprintln!("skipping paced-rehearsal app test: capture corpus absent");
        return;
    };
    let mut source = FixtureSource::from_captures(&dir).unwrap();
    let config = example_config();
    let report = install_rehearsal(&mut source, &config, 60.0, NOW).await.unwrap();

    let pre = preflight(
        &source,
        &config,
        Duration::from_secs(5),
        false,
        classify_fixture_error,
        &PreflightEnv::silent(),
    )
    .await;
    assert!(pre.fatal.is_none(), "{}", pre.render());
    let mut state = AppState::new(config, pre.writes_armed, pre.into_bootstraps(), NOW);

    let events: Vec<BracketId> = state.brackets.iter().map(|b| b.state.id.clone()).collect();
    let poller_config = PollerConfig {
        interval: Duration::from_secs(30),
        request_timeout: Duration::from_secs(5),
        concurrency: 3,
    };
    let classify = |e: &_| -> PollFailure { classify_fixture_error(e) };
    let completed = |state: &AppState| -> usize {
        state
            .brackets
            .iter()
            .flat_map(|b| &b.state.sets)
            .filter(|s| s.is_completed())
            .count()
    };

    // t0: the initial world — nothing completed, but callable (the whole
    // point of a rehearsal is a drillable opening rush).
    source.rewind_clock(Duration::ZERO);
    for result in poll_cycle(&source, &events, 1, &poller_config, &classify, &Default::default()).await {
        update(&mut state, Msg::Poll(result), NOW + 1_000);
    }
    assert_eq!(completed(&state), 0);
    assert!(!state.world.queue.is_empty(), "frame 0 must offer callable sets");

    // Mid-script: completions have arrived.
    let wall = (report.finishes_at - report.started_at) as u64;
    source.rewind_clock(Duration::from_millis(wall / 2));
    for result in poll_cycle(&source, &events, 2, &poller_config, &classify, &Default::default()).await {
        update(&mut state, Msg::Poll(result), NOW + 2_000);
    }
    let midway = completed(&state);
    assert!(midway > 50, "expected substantial mid-script progress, got {midway}");

    // Past the end: the whole corpus has played out.
    source.rewind_clock(Duration::from_millis(wall + 1_000));
    for result in poll_cycle(&source, &events, 3, &poller_config, &classify, &Default::default()).await {
        update(&mut state, Msg::Poll(result), NOW + 3_000);
    }
    let done = completed(&state);
    assert!(done > midway, "progress continued: {midway} -> {done}");
    assert_eq!(state.world.summaries.len(), 7);

    // And the TUI renders the played-out world.
    update(&mut state, Msg::Tick, NOW + 4_000);
    let mut terminal = Terminal::new(TestBackend::new(130, 40)).unwrap();
    terminal.draw(|frame| ui::draw(frame, &state, NOW + 4_000)).unwrap();
}

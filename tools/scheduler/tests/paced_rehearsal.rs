//! Env-gated (like fixture_replay.rs): builds the paced rehearsal over the
//! real S1 capture corpus with the shipped example config, then fast-forwards
//! the shared clock and checks the played-out world.

use std::{env, path::PathBuf, time::Duration};

use bracket_tools_scheduler::{
    fixture_source::FixtureSource, model::live_sets_from_schema, rehearsal::install_rehearsal, set_source::SetSource, SchedulerConfig,
};

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

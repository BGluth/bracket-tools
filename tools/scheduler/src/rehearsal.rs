//! The paced `--simulate` rehearsal driver: turns a static capture corpus
//! into a wall-clock-paced scripted timeline. The captured world is played
//! forward with the config's own setups and duration priors
//! (`simulate_recorded`), and each completion becomes a timestamped frame
//! served back through [`FixtureSource`]'s paced mode.
//!
//! Frames are completions-only: an incomplete set never carries
//! `started_at`, so the desk sees results arrive (as web-UI entry does live)
//! but never phantom remote-active sets for calls it didn't make. If the
//! operator follows the tool's recommendations the script and the desk stay
//! roughly in step; deviations surface as ordinary no-shows and deviation
//! notices — themselves worth rehearsing.

use std::{
    collections::{HashMap, HashSet},
    fmt::Write as _,
};

use bracket_tools_startgg_schema::get_sets_for_event;
use thiserror::Error;

use crate::{
    config::SchedulerConfig,
    conflict::{AliasMap, PlayerFlags, SetupBoard, Tombstones, UnixMillis},
    duration::DurationModel,
    fixture_source::{schema_set_from_live, FixtureError, FixtureSource},
    model::{live_sets_from_schema, phase_groups_from_schema, BracketId, EntrantId, LiveSet, PlayerId, Prereq, SetId, SlotOccupant},
    set_source::SetSource,
    simulator::{simulate_recorded, ScriptFrame, SimBracket, SimWorld},
};

/// Materialized rehearsal ids start here: far above live set ids, still
/// numeric (the writer only arms mutations for numeric ids).
const REHEARSAL_ID_BASE: u64 = 9_900_000_000;
const REHEARSAL_IDS_PER_EVENT: u64 = 1_000_000;
/// Synthetic drop-in entrant/player ids (see [`fill_dangling_slots`]).
const DROP_IN_ID_BASE: u64 = 9_890_000_000;
/// Live-observed COMPLETED state (ActivityState ordinal 3).
const COMPLETED_STATE_INT: i32 = 3;

#[derive(Debug, Error)]
pub enum RehearsalError {
    #[error("config bracket {slug:?} is not in the capture dir: {source}")]
    MissingEvent {
        slug: String,
        #[source]
        source: FixtureError,
    },

    #[error("--pace must be positive, got {0}")]
    InvalidSpeed(f64),
}

/// What the generator installed, printed before the TUI takes the screen.
#[derive(Debug)]
pub struct RehearsalReport {
    pub speed: f64,
    pub started_at: UnixMillis,
    /// Wall-clock unix millis when the scripted timeline runs dry.
    pub finishes_at: UnixMillis,
    /// Frames installed per bracket (initial world included).
    pub frames: Vec<(BracketId, usize)>,
    /// Brackets the sim could not play to completion: their timelines end
    /// with sets still open.
    pub blocked: Vec<BracketId>,
}

impl RehearsalReport {
    pub fn render(&self) -> String {
        let mut out = String::new();
        let total: usize = self.frames.iter().map(|(_, n)| n).sum();
        let wall_mins = (self.finishes_at - self.started_at) / 60_000;
        let sim_mins = (wall_mins as f64 * self.speed) as i64;
        let _ = writeln!(
            out,
            "rehearsal: {} brackets, {} frames at {}x — ~{}m of tournament plays back in ~{}m",
            self.frames.len(),
            total,
            self.speed,
            sim_mins,
            wall_mins,
        );
        for id in &self.blocked {
            let _ = writeln!(
                out,
                "  WARNING {}: could not script to completion — its timeline ends with open sets",
                id.0
            );
        }
        out
    }
}

/// Builds and installs a paced rehearsal over `source`'s registered events —
/// one scripted timeline per configured bracket, released on the shared
/// clock starting at the first poll.
pub async fn install_rehearsal(
    source: &mut FixtureSource,
    config: &SchedulerConfig,
    speed: f64,
    now_millis: UnixMillis,
) -> Result<RehearsalReport, RehearsalError> {
    if speed <= 0.0 || !speed.is_finite() {
        return Err(RehearsalError::InvalidSpeed(speed));
    }

    let (initial, world, durations) = load_world(source, config, now_millis).await?;
    let (outcome, frames) = simulate_recorded(&world, &durations);
    // Not overall_finish: blocked brackets have no finish entry, but their
    // frames still play out.
    let last_frame_at = frames.iter().map(|f| f.at).max().unwrap_or(now_millis);

    let mut by_bracket: HashMap<BracketId, Vec<ScriptFrame>> = HashMap::new();
    for frame in frames {
        by_bracket.entry(frame.bracket.clone()).or_default().push(frame);
    }

    let anchor_secs = now_millis / 1000;
    let mut report_frames = Vec::new();
    for (slug, first) in initial {
        let id = BracketId(slug.clone());
        let mut timeline = vec![(0, to_schema(&first, anchor_secs, speed))];
        for frame in coalesce(by_bracket.remove(&id).unwrap_or_default()) {
            let offset = wall_offset(frame.at, now_millis, speed);
            timeline.push((offset, to_schema(&frame.sets, anchor_secs, speed)));
        }
        report_frames.push((id, timeline.len()));
        source.set_timeline(&slug, timeline);
    }

    Ok(RehearsalReport {
        speed,
        started_at: now_millis,
        finishes_at: now_millis + wall_offset(last_frame_at, now_millis, speed),
        frames: report_frames,
        blocked: outcome.blocked,
    })
}

/// Fetches every configured event from the fixtures and folds the config
/// into a simulatable world (numeric ids, open-now brackets, config board
/// and priors). Shared with the `--autoplay` replay generator.
pub(crate) async fn load_world(
    source: &FixtureSource,
    config: &SchedulerConfig,
    now_millis: UnixMillis,
) -> Result<(Vec<(String, Vec<LiveSet>)>, SimWorld, DurationModel), RehearsalError> {
    let mut durations = DurationModel::new();
    let mut initial = Vec::new();
    let mut brackets = Vec::new();
    let mut drop_ins = DROP_IN_ID_BASE;
    for (ix, bracket) in config.brackets.iter().enumerate() {
        let missing = |source| RehearsalError::MissingEvent {
            slug: bracket.slug.clone(),
            source,
        };
        let sets = source.fetch_event_sets(&bracket.slug).await.map_err(missing)?;
        let structure = source.fetch_event_structure(&bracket.slug).await.map_err(missing)?;
        let (live, _warnings, _skipped) = live_sets_from_schema(sets);
        let (groups, _) = phase_groups_from_schema(&structure);
        let mut live = ensure_numeric_ids(live, ix);
        fill_dangling_slots(&mut live, &mut drop_ins);

        durations.configure_bracket(bracket.id(), bracket.duration_prior_secs, bracket.prior_weight);
        brackets.push(SimBracket {
            id: bracket.id(),
            sets: live.clone(),
            groups,
            mode: bracket.mode,
            // A rehearsal world is live now; real start times were cleared
            // from the served structures by set_timeline.
            start_at: None,
            held: false,
            pool: bracket.pool.clone(),
        });
        initial.push((bracket.slug.clone(), live));
    }

    let world = SimWorld {
        brackets,
        board: SetupBoard::new(&config.setups),
        flags: PlayerFlags::default(),
        tombstones: Tombstones::default(),
        called_ints: config.known_called_state_int.into_iter().collect(),
        aliases: AliasMap::build(&config.player_aliases),
        soft_busy: Vec::new(),
        last_completed: HashMap::new(),
        rest_window_secs: config.rest_window_secs,
        sim: config.sim.clone(),
        now_millis,
    };
    Ok((initial, world, durations))
}

/// Renumbers preview ids to fresh numeric ones (rewriting matching prereq
/// edges), as bracket start does live. Already-numeric ids are kept — the
/// writer can only mutate numeric ids, and a rehearsal should exercise it.
fn ensure_numeric_ids(sets: Vec<LiveSet>, event_ix: usize) -> Vec<LiveSet> {
    let mut next = REHEARSAL_ID_BASE + event_ix as u64 * REHEARSAL_IDS_PER_EVENT;
    let mut mapping: HashMap<String, String> = HashMap::new();
    for set in &sets {
        if set.id.0.parse::<u64>().is_err() {
            mapping.insert(set.id.0.clone(), next.to_string());
            next += 1;
        }
    }
    if mapping.is_empty() {
        return sets;
    }
    sets.into_iter()
        .map(|mut set| {
            if let Some(numeric) = mapping.get(&set.id.0) {
                set.id = SetId(numeric.clone());
            }
            for slot in &mut set.slots {
                if let Some(Prereq::Set { id, .. }) = &mut slot.prereq {
                    if let Some(numeric) = mapping.get(&id.0) {
                        *id = SetId(numeric.clone());
                    }
                }
            }
            set
        })
        .collect()
}

/// Live brackets omit degenerate (bye-heavy) sets while other sets still
/// reference them as prereqs — the server fills those slots itself, so the
/// script has to stand in for it: each empty slot whose prereq points at an
/// omitted set gets a synthetic drop-in occupant. Without this the losers
/// side starves and the timeline stops mid-bracket. Ids and names are unique
/// across the whole rehearsal so the conflict index and the preflight
/// identity scan stay quiet.
fn fill_dangling_slots(sets: &mut [LiveSet], next_id: &mut u64) {
    let mut dangling = Vec::new();
    {
        let known: HashSet<&str> = sets.iter().map(|s| s.id.0.as_str()).collect();
        for (s, set) in sets.iter().enumerate() {
            for (i, slot) in set.slots.iter().enumerate() {
                let unresolvable = matches!(&slot.prereq, Some(Prereq::Set { id, .. }) if !known.contains(id.0.as_str()));
                if unresolvable && slot.occupant.is_none() {
                    dangling.push((s, i));
                }
            }
        }
    }

    for (s, i) in dangling {
        let id = *next_id;
        *next_id += 1;
        sets[s].slots[i].occupant = Some(SlotOccupant {
            entrant_id: EntrantId(id.to_string()),
            display_name: format!("drop-in {}", id - DROP_IN_ID_BASE + 1),
            is_disqualified: false,
            player_ids: vec![PlayerId(id.to_string())],
        });
        if sets[s].has_placeholder && sets[s].all_slots_occupied() {
            sets[s].has_placeholder = false;
        }
    }
}

/// Cascade frames share a timestamp; only the last one per instant matters.
fn coalesce(frames: Vec<ScriptFrame>) -> Vec<ScriptFrame> {
    let mut out: Vec<ScriptFrame> = Vec::with_capacity(frames.len());
    for frame in frames {
        match out.last_mut() {
            Some(prev) if prev.at == frame.at => *prev = frame,
            _ => out.push(frame),
        }
    }
    out
}

/// Sim-time → wall milliseconds after the rehearsal start, compressed by
/// `speed`.
fn wall_offset(sim_millis: UnixMillis, sim_start: UnixMillis, speed: f64) -> i64 {
    (((sim_millis - sim_start) as f64) / speed) as i64
}

/// Frame fix-ups + schema conversion: completed sets read as live COMPLETED;
/// incomplete sets never carry `started_at` (completions-only script); sim
/// timestamps map back onto the wall clock so a 1x rehearsal reads exactly
/// live and a fast one stays self-consistent.
fn to_schema(sets: &[LiveSet], anchor_secs: i64, speed: f64) -> Vec<get_sets_for_event::Set> {
    sets.iter()
        .map(|set| {
            let mut set = set.clone();
            if set.is_completed() {
                set.state_int = Some(COMPLETED_STATE_INT);
            } else {
                set.started_at = None;
            }
            set.started_at = set.started_at.map(|t| rebase(t, anchor_secs, speed));
            set.completed_at = set.completed_at.map(|t| rebase(t, anchor_secs, speed));
            schema_set_from_live(&set)
        })
        .collect()
}

/// Rebase a sim timestamp (unix secs) onto the wall clock; real history from
/// before the rehearsal started passes through untouched.
fn rebase(ts_secs: i64, anchor_secs: i64, speed: f64) -> i64 {
    if ts_secs <= anchor_secs {
        return ts_secs;
    }
    anchor_secs + (((ts_secs - anchor_secs) as f64) / speed) as i64
}

#[cfg(test)]
mod tests {
    use std::{slice::from_ref, time::Duration};

    use super::{install_rehearsal, RehearsalError};
    use crate::{
        config::{BracketConfig, SchedulerConfig, SetupId},
        fixture_source::FixtureSource,
        model::live_sets_from_schema,
        set_source::SetSource,
        synth::make_de_bracket,
    };

    const NOW: i64 = 1_751_000_000_000;
    const SLUG: &str = "tournament/synth/event/melee-singles";

    fn bracket_config(slug: &str, pool: &[SetupId]) -> BracketConfig {
        BracketConfig {
            slug: slug.to_owned(),
            expected_kind: None,
            mode: Default::default(),
            start_at_override: None,
            duration_prior_secs: 480,
            prior_weight: 3.0,
            pool: pool.to_vec(),
        }
    }

    fn synth_setup(slugs: &[&str]) -> (FixtureSource, SchedulerConfig) {
        let setups = [SetupId(1), SetupId(2)];
        let mut source = FixtureSource::new();
        let mut config = SchedulerConfig {
            setups: setups.to_vec(),
            ..Default::default()
        };
        for (ix, slug) in slugs.iter().enumerate() {
            let bracket = make_de_bracket(1001 + ix as u64 * 1000, 4);
            source.add_synth_event(slug, from_ref(&bracket.info), vec![bracket.sets]);
            config.brackets.push(bracket_config(slug, &setups));
        }
        (source, config)
    }

    #[tokio::test]
    async fn scripts_a_de4_to_completion_at_speed() {
        let (mut source, config) = synth_setup(&[SLUG]);
        let report = install_rehearsal(&mut source, &config, 60.0, NOW).await.unwrap();

        // 6 completions on 2 setups = 4 sequential rounds of 480s, played at
        // 60x: 32 wall seconds. Same-instant completions coalesce, so the
        // timeline is the initial world + 4 round instants.
        assert_eq!(report.finishes_at - report.started_at, 32_000);
        assert_eq!(report.frames, vec![(crate::model::BracketId(SLUG.to_owned()), 5)]);
        assert!(report.blocked.is_empty());

        // t0: the initial world, materialized to numeric ids, none started.
        let (live, warnings, skipped) = live_sets_from_schema(source.fetch_event_sets(SLUG).await.unwrap());
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(skipped.is_empty(), "{skipped:?}");
        assert!(live.iter().all(|s| s.id.0.parse::<u64>().is_ok()), "preview ids materialized");
        assert!(live.iter().all(|s| !s.is_completed() && s.started_at.is_none()));

        // Mid-script: some but not all done, and nothing incomplete reads as
        // remotely active.
        source.rewind_clock(Duration::from_secs(17));
        let (live, _, _) = live_sets_from_schema(source.fetch_event_sets(SLUG).await.unwrap());
        let done = live.iter().filter(|s| s.is_completed()).count();
        assert!(done > 0 && done < 6, "expected mid-script, got {done} completed");
        assert!(live.iter().all(|s| s.is_completed() || !s.is_remotely_active()));

        // Past the end: everything but the un-fired reset is complete, with
        // wall-clock-plausible completion times.
        source.rewind_clock(Duration::from_secs(60));
        let (live, _, _) = live_sets_from_schema(source.fetch_event_sets(SLUG).await.unwrap());
        assert_eq!(live.iter().filter(|s| s.is_completed()).count(), 6);
        let anchor = NOW / 1000;
        for set in live.iter().filter(|s| s.is_completed()) {
            let at = set.completed_at.unwrap();
            assert!((anchor..=anchor + 32).contains(&at), "completed_at {at} outside the wall window");
        }
    }

    #[tokio::test]
    async fn shared_players_serialize_across_scripted_brackets() {
        let second = "tournament/synth/event/ultimate-singles";
        let (mut source, config) = synth_setup(&[SLUG, second]);
        let report = install_rehearsal(&mut source, &config, 60.0, NOW).await.unwrap();

        assert!(report.blocked.is_empty());
        // Both brackets script beyond their initial frame.
        assert!(report.frames.iter().all(|(_, n)| *n == 5), "{:?}", report.frames);
        // Synth brackets share P1..P4, so the two DE-4s cannot overlap at
        // all: 8 sequential sets, not 4.
        assert_eq!(report.finishes_at - report.started_at, 64_000);
    }

    #[tokio::test]
    async fn config_slug_missing_from_captures_is_an_error() {
        let (mut source, mut config) = synth_setup(&[SLUG]);
        config.brackets.push(bracket_config("tournament/other/event/x", &[SetupId(1)]));
        let err = install_rehearsal(&mut source, &config, 8.0, NOW).await.unwrap_err();
        assert!(matches!(err, RehearsalError::MissingEvent { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn nonpositive_speed_is_rejected() {
        let (mut source, config) = synth_setup(&[SLUG]);
        for bad in [0.0, -2.0, f64::NAN] {
            let err = install_rehearsal(&mut source, &config, bad, NOW).await.unwrap_err();
            assert!(matches!(err, RehearsalError::InvalidSpeed(_)));
        }
    }
}

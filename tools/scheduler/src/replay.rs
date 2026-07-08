//! The `--autoplay` replay: the simulator plays the offline world by itself —
//! committing the greedy ranker's top call whenever a setup frees — and the
//! run is rendered as an ASCII animation plus a decision log.
//!
//! The rendered file is both human-readable (`cat`/`less` shows every frame
//! in order, then the decision log and summary) and machine-playable: frames
//! start with a `▶` line, and `scheduler --replay <file>` pages through them
//! in the terminal like a flipbook.

use std::{
    collections::HashMap,
    fmt::Write as _,
    fs,
    io::{self, Write as _},
    path::Path,
    thread,
    time::Duration,
};

use crate::{
    config::{SchedulerConfig, SetupId},
    conflict::UnixMillis,
    fixture_source::FixtureSource,
    model::BracketId,
    rehearsal::{load_world, RehearsalError},
    simulator::{simulate_autoplay, ReplayEvent, SimOutcome},
};

/// Every frame's first line starts with this (the playback split marker).
const FRAME_MARK: &str = "▶";
const BAR_WIDTH: usize = 20;
const NAME_WIDTH: usize = 24;

/// One generated auto-play run, ready to render.
#[derive(Debug)]
pub struct Replay {
    pub started_at: UnixMillis,
    pub outcome: SimOutcome,
    pub events: Vec<ReplayEvent>,
    /// (bracket, incomplete sets at the start) — progress-bar denominators.
    pub brackets: Vec<(BracketId, usize)>,
    pub setups: Vec<SetupId>,
}

/// Plays the configured events forward under the sim's own decisions.
pub async fn generate_replay(source: &FixtureSource, config: &SchedulerConfig, now_millis: UnixMillis) -> Result<Replay, RehearsalError> {
    let (_, world, durations) = load_world(source, config, now_millis).await?;
    let brackets = world
        .brackets
        .iter()
        .map(|b| (b.id.clone(), b.sets.iter().filter(|s| !s.is_completed()).count()))
        .collect();
    let (outcome, events) = simulate_autoplay(&world, &durations);
    Ok(Replay {
        started_at: now_millis,
        outcome,
        events,
        brackets,
        setups: config.setups.clone(),
    })
}

impl Replay {
    /// The stdout digest printed after `--autoplay` (also the file's tail).
    pub fn summary(&self) -> String {
        let mut out = String::new();
        let calls = self.events.iter().filter(|e| matches!(e, ReplayEvent::Call { .. })).count();
        let results = self.events.len() - calls;
        let makespan = self.outcome.overall_finish - self.started_at;
        let _ = writeln!(
            out,
            "autoplay: {} calls, {} results across {} bracket(s) on {} setups — makespan {}",
            calls,
            results,
            self.brackets.len(),
            self.setups.len(),
            fmt_t(makespan),
        );
        for (id, finish) in ordered_finishes(&self.outcome) {
            let _ = writeln!(out, "  {} finishes at T+{}", short_name(&id), fmt_t(finish - self.started_at));
        }
        for id in &self.outcome.blocked {
            let _ = writeln!(out, "  WARNING {}: starved — the sim could not finish it", id.0);
        }
        out
    }
}

/// Renders the whole replay file: header, one frame per event, decision log,
/// summary.
pub fn render_replay(replay: &Replay) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "scheduler autoplay replay");
    let _ = writeln!(out, "=========================");
    for (id, sets) in &replay.brackets {
        let _ = writeln!(out, "  {}: {} sets to play", short_name(id), sets);
    }
    let _ = writeln!(out, "  setups: {}", replay.setups.len());
    let _ = writeln!(
        out,
        "\nEvery `{FRAME_MARK}` line begins one frame; play with: scheduler --replay <this file>\n"
    );

    let mut tracker = Tracker::new(replay);
    for event in &replay.events {
        out.push_str(&tracker.frame(event, replay));
        out.push('\n');
    }

    let _ = writeln!(out, "\ndecision log");
    let _ = writeln!(out, "------------");
    for event in &replay.events {
        if let ReplayEvent::Call {
            at,
            setup,
            bracket,
            players,
            round_text,
            components,
            ..
        } = event
        {
            let _ = writeln!(
                out,
                "T+{} setup {}: {players} — {round_text} ({}) · depth {} iron {} unblk {} wait {}",
                fmt_t(at - replay.started_at),
                setup.0,
                short_name(bracket),
                components.depth,
                components.ironman,
                components.unblock,
                fmt_t(components.wait_secs * 1000),
            );
        }
    }
    out.push('\n');
    out.push_str(&replay.summary());
    out
}

/// Board + progress state carried across frames (rebuilt from the events).
struct Tracker {
    /// setup -> (bracket, players) currently on it.
    assignments: HashMap<SetupId, (BracketId, crate::model::SetKey, String)>,
    /// bracket -> (done, remaining) so far.
    progress: HashMap<BracketId, (usize, usize)>,
    order: Vec<BracketId>,
}

impl Tracker {
    fn new(replay: &Replay) -> Self {
        Self {
            assignments: HashMap::new(),
            progress: replay.brackets.iter().map(|(id, sets)| (id.clone(), (0, *sets))).collect(),
            order: replay.brackets.iter().map(|(id, _)| id.clone()).collect(),
        }
    }

    fn frame(&mut self, event: &ReplayEvent, replay: &Replay) -> String {
        let mut out = String::new();
        match event {
            ReplayEvent::Call {
                at,
                setup,
                bracket,
                key,
                players,
                round_text,
                components,
                est_finish,
            } => {
                self.assignments.insert(*setup, (bracket.clone(), key.clone(), players.clone()));
                let _ = writeln!(out, "{FRAME_MARK} T+{} · CALL", fmt_t(at - replay.started_at));
                self.board_and_bars(&mut out, replay);
                let _ = writeln!(out, "→ CALL setup {}: {players} — {round_text} ({})", setup.0, short_name(bracket));
                let _ = writeln!(
                    out,
                    "  why: depth {} · ironman {} · unblocks {} · waited {} · est done T+{}",
                    components.depth,
                    components.ironman,
                    components.unblock,
                    fmt_t(components.wait_secs * 1000),
                    fmt_t(est_finish - replay.started_at),
                );
            }
            ReplayEvent::Complete {
                at,
                bracket,
                key,
                players,
                winner,
                remaining,
            } => {
                self.assignments.retain(|_, (b, k, _)| !(b == bracket && k == key));
                let entry = self.progress.entry(bracket.clone()).or_insert((0, *remaining));
                entry.0 += 1;
                entry.1 = *remaining;
                let _ = writeln!(out, "{FRAME_MARK} T+{} · RESULT", fmt_t(at - replay.started_at));
                self.board_and_bars(&mut out, replay);
                let versus = if players.is_empty() { "(walkover)" } else { players.as_str() };
                let _ = writeln!(
                    out,
                    "✓ {} {}: {} wins ({versus}) — {} left",
                    short_name(bracket),
                    key.identifier,
                    winner,
                    remaining
                );
            }
        }
        out
    }

    fn board_and_bars(&self, out: &mut String, replay: &Replay) {
        let mut line = String::from("setups ");
        for setup in &replay.setups {
            let label = self
                .assignments
                .get(setup)
                .map(|(_, _, players)| truncate(players, 16))
                .unwrap_or_else(|| "—".to_owned());
            let _ = write!(line, " {}:{label}", setup.0);
        }
        let _ = writeln!(out, "{line}");
        for id in &self.order {
            let (done, remaining) = self.progress.get(id).copied().unwrap_or((0, 0));
            let total = done + remaining;
            let filled = (done * BAR_WIDTH).checked_div(total).unwrap_or(BAR_WIDTH);
            let _ = writeln!(
                out,
                "{:<NAME_WIDTH$} [{}{}] {done}/{total}",
                truncate(short_name(id), NAME_WIDTH),
                "█".repeat(filled),
                "░".repeat(BAR_WIDTH - filled),
            );
        }
    }
}

/// Plays a rendered replay file in the terminal, one frame per cadence tick.
pub fn play_replay(path: &Path, frame_ms: u64) -> io::Result<()> {
    let text = fs::read_to_string(path)?;
    let frames = split_frames(&text);
    let mut stdout = io::stdout();
    for frame in &frames {
        // Clear + home, then the frame (plain ANSI; no raw mode needed).
        write!(stdout, "\x1b[2J\x1b[H{frame}")?;
        stdout.flush()?;
        thread::sleep(Duration::from_millis(frame_ms));
    }
    Ok(())
}

/// Splits a rendered replay into playable chunks: the header, then one chunk
/// per `▶` frame (the trailing log/summary rides with the last frame).
pub fn split_frames(text: &str) -> Vec<String> {
    let mut frames = vec![String::new()];
    for line in text.lines() {
        if line.starts_with(FRAME_MARK) {
            frames.push(String::new());
        }
        let current = frames.last_mut().expect("never empty");
        current.push_str(line);
        current.push('\n');
    }
    frames.retain(|f| !f.trim().is_empty());
    frames
}

/// Sim-duration formatting: `h:mm` past an hour, else `Nm`, else `Ns`.
fn fmt_t(millis: i64) -> String {
    let secs = millis / 1000;
    if secs >= 3600 {
        format!("{}:{:02}h", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

fn ordered_finishes(outcome: &SimOutcome) -> Vec<(BracketId, UnixMillis)> {
    let mut finishes: Vec<(BracketId, UnixMillis)> = outcome.per_bracket_finish.iter().map(|(k, v)| (k.clone(), *v)).collect();
    finishes.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    finishes
}

fn short_name(id: &BracketId) -> &str {
    id.0.rsplit('/').next().unwrap_or(&id.0)
}

fn truncate(name: &str, max: usize) -> String {
    if name.chars().count() <= max {
        name.to_owned()
    } else {
        let cut: String = name.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::{generate_replay, render_replay, split_frames};
    use crate::{fixture_source::FixtureSource, simulator::ReplayEvent};

    const NOW: i64 = 1_751_000_000_000;

    #[tokio::test]
    async fn autoplay_plays_a_synth_world_to_completion() {
        let source = FixtureSource::from_synth_spec("de:8,de:4").unwrap();
        let (config, skipped) = source.derived_config();
        assert!(skipped.is_empty());

        let replay = generate_replay(&source, &config, NOW).await.unwrap();
        assert!(replay.outcome.blocked.is_empty(), "{:?}", replay.outcome.blocked);
        let calls = replay.events.iter().filter(|e| matches!(e, ReplayEvent::Call { .. })).count();
        let results = replay.events.iter().filter(|e| matches!(e, ReplayEvent::Complete { .. })).count();
        assert!(calls > 0, "the sim made calls");
        // Every playable set completes; the un-fired GF resets never do, and
        // walkover completions can outnumber calls.
        assert!(results >= calls, "calls {calls} results {results}");
        let last_remaining = replay
            .events
            .iter()
            .rev()
            .find_map(|e| match e {
                ReplayEvent::Complete { remaining, .. } => Some(*remaining),
                _ => None,
            })
            .unwrap();
        assert!(last_remaining <= 1, "only an un-fired reset may stay open, got {last_remaining}");
    }

    #[tokio::test]
    async fn rendered_replay_is_playable_and_explains_decisions() {
        let source = FixtureSource::from_synth_spec("de:8").unwrap();
        let (config, _) = source.derived_config();
        let replay = generate_replay(&source, &config, NOW).await.unwrap();

        let text = render_replay(&replay);
        assert!(text.contains("→ CALL setup"), "{text}");
        assert!(text.contains("why: depth"), "{text}");
        assert!(text.contains("decision log"), "{text}");
        assert!(text.contains("autoplay:"), "{text}");

        let frames = split_frames(&text);
        // Header + one frame per event (log/summary ride with the last).
        assert_eq!(frames.len(), replay.events.len() + 1);
        assert!(frames[0].contains("scheduler autoplay replay"));
        assert!(frames[1].starts_with('▶'), "{}", frames[1]);
    }
}

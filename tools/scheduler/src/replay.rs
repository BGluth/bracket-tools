//! The `--autoplay` replay: the simulator plays the offline world by itself —
//! committing the greedy ranker's top call whenever a setup frees — and the
//! run is rendered as an ASCII animation plus a decision log.
//!
//! The rendered file is both human-readable (`cat`/`less` shows every frame
//! in order, then the decision log and summary) and machine-playable: frames
//! start with a `▶` line, and `scheduler --replay <file>` pages through them
//! in the terminal like a flipbook — auto-advancing, with arrow keys to step
//! back and forth at your own pace.

use std::{
    collections::HashMap,
    env,
    fmt::Write as _,
    fs,
    io::{self, IsTerminal, Write as _},
    path::Path,
    thread,
    time::Duration,
};

use crossterm::{
    cursor::Hide,
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{enable_raw_mode, EnterAlternateScreen},
};

use crate::{
    config::{SchedulerConfig, SetupId},
    conflict::UnixMillis,
    fixture_source::FixtureSource,
    graph::BracketGraph,
    model::BracketId,
    ranker::ScoreComponents,
    rehearsal::{load_world, RehearsalError},
    simulator::{simulate_autoplay, ReplayEvent, SetContext, SimOutcome},
    terminal::restore_terminal,
};

/// Every frame's first line starts with this (the playback split marker).
const FRAME_MARK: &str = "▶";
const BAR_WIDTH: usize = 20;
const NAME_WIDTH: usize = 24;

const RESET: &str = "\x1b[0m";
const BOLD_YELLOW: &str = "\x1b[1;33m";
const BOLD_GREEN: &str = "\x1b[1;32m";
const CYAN: &str = "\x1b[36m";
const BOLD_RED: &str = "\x1b[1;31m";
const GREEN: &str = "\x1b[32m";
const DIM: &str = "\x1b[2m";

/// One bracket's shape when the replay starts.
#[derive(Debug)]
pub struct BracketOpening {
    pub id: BracketId,
    /// Incomplete sets — the progress-bar denominator.
    pub sets_to_play: usize,
    /// Sequential stages left on the critical path: the depth term that
    /// dominates call order.
    pub critical_path: u32,
}

/// One generated auto-play run, ready to render.
#[derive(Debug)]
pub struct Replay {
    pub started_at: UnixMillis,
    pub outcome: SimOutcome,
    pub events: Vec<ReplayEvent>,
    pub brackets: Vec<BracketOpening>,
    pub setups: Vec<SetupId>,
}

/// Plays the configured events forward under the sim's own decisions.
pub async fn generate_replay(source: &FixtureSource, config: &SchedulerConfig, now_millis: UnixMillis) -> Result<Replay, RehearsalError> {
    let (_, world, durations) = load_world(source, config, now_millis).await?;
    let brackets = world
        .brackets
        .iter()
        .map(|b| {
            let (graph, _) = BracketGraph::build(&b.sets, &b.groups);
            BracketOpening {
                id: b.id.clone(),
                sets_to_play: b.sets.iter().filter(|s| !s.is_completed()).count(),
                critical_path: graph.remaining_critical_path(),
            }
        })
        .collect();
    let (outcome, events) = simulate_autoplay(&world, &durations);
    let setups = world.board.setups().iter().map(|s| s.id).collect();
    Ok(Replay {
        started_at: now_millis,
        outcome,
        events,
        brackets,
        setups,
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
    for opening in &replay.brackets {
        let _ = writeln!(
            out,
            "  {}: {} sets to play · critical path {}",
            short_name(&opening.id),
            opening.sets_to_play,
            opening.critical_path
        );
    }
    let _ = writeln!(out, "  setups: {}", replay.setups.len());
    let _ = writeln!(out, "\ncall policy: longest remaining critical path wins (depth ≫ all else);");
    let _ = writeln!(out, "ties go to the busiest player (iron), then sets unblocked, then longest");
    let _ = writeln!(out, "wait. Expect the deepest bracket to flood the setups first — that is the");
    let _ = writeln!(out, "depth term working, not a bug.");
    let _ = writeln!(
        out,
        "\nEvery `{FRAME_MARK}` line begins one frame; play with: scheduler --replay <this file>"
    );
    let _ = writeln!(out, "(auto-advances; space pauses, arrow keys step back/forward, q quits)\n");

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
            runner_up,
            ..
        } = event
        {
            let _ = write!(
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
            if let Some(ru) = runner_up {
                let _ = write!(out, " › over {} d{}", short_name(&ru.bracket), ru.components.depth);
            }
            out.push('\n');
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
            progress: replay.brackets.iter().map(|b| (b.id.clone(), (0, b.sets_to_play))).collect(),
            order: replay.brackets.iter().map(|b| b.id.clone()).collect(),
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
                runner_up,
                context,
                est_finish,
            } => {
                self.assignments.insert(*setup, (bracket.clone(), key.clone(), players.clone()));
                let _ = writeln!(out, "{FRAME_MARK} T+{} · CALL", fmt_t(at - replay.started_at));
                self.board_and_bars(&mut out, replay);
                let _ = writeln!(out, "→ CALL setup {}: {players} — {round_text} ({})", setup.0, short_name(bracket));
                let _ = writeln!(
                    out,
                    "  why: {} · est done T+{}",
                    components_text(components),
                    fmt_t(est_finish - replay.started_at),
                );
                if let Some(ru) = runner_up {
                    let _ = writeln!(
                        out,
                        "  over: {} — {} ({}): {} — {}",
                        ru.players,
                        ru.round_text,
                        short_name(&ru.bracket),
                        components_text(&ru.components),
                        decisive_term(components, &ru.components),
                    );
                }
                out.push_str(&context_lines(context));
            }
            ReplayEvent::Complete {
                at,
                bracket,
                key,
                players,
                winner,
                winner_to,
                loser_to,
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
                let mut parts = Vec::new();
                if let Some(to) = winner_to {
                    let name = if winner.is_empty() { "winner" } else { winner.as_str() };
                    parts.push(format!("{name} → {to}"));
                }
                if let Some(to) = loser_to {
                    parts.push(format!("loser → {to}"));
                }
                if !parts.is_empty() {
                    let _ = writeln!(out, "  └ {}", parts.join(" · "));
                }
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

/// The frame-line rendering of a candidate's score ingredients.
fn components_text(c: &ScoreComponents) -> String {
    format!(
        "depth {} · ironman {} · unblocks {} · waited {}",
        c.depth,
        c.ironman,
        c.unblock,
        fmt_t(c.wait_secs * 1000)
    )
}

/// Which score term separated a call from its runner-up (the terms are
/// compared in weight order, so the first difference decided it).
fn decisive_term(top: &ScoreComponents, other: &ScoreComponents) -> &'static str {
    if top.depth != other.depth {
        "depth decided"
    } else if top.ironman != other.ironman {
        "ironman decided"
    } else if top.unblock != other.unblock {
        "unblocks decided"
    } else if top.wait_secs != other.wait_secs {
        "wait decided"
    } else {
        "dead tie; deterministic order"
    }
}

/// The zoomed-in bracket neighborhood: where each player came from, where
/// the winner and loser go next.
fn context_lines(context: &SetContext) -> String {
    let mut out = String::new();
    let destinations = destination_line(context);
    let n = context.sources.len();
    for (i, source) in context.sources.iter().enumerate() {
        let last = destinations.is_none() && i + 1 == n;
        let connector = match (i, last, n) {
            (_, true, 1) => '─',
            (0, _, _) => '┌',
            (_, true, _) => '└',
            _ => '├',
        };
        let _ = writeln!(out, "  {connector} {source}");
    }
    if let Some(destinations) = destinations {
        let _ = writeln!(out, "  └ {destinations}");
    }
    out
}

fn destination_line(context: &SetContext) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(to) = &context.winner_to {
        parts.push(format!("winner → {to}"));
    }
    if let Some(to) = &context.loser_to {
        parts.push(format!("loser → {to}"));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// Plays a rendered replay file in the terminal. On a tty this is
/// interactive (auto-advance with pause/step controls); piped output falls
/// back to a fixed-cadence dump.
pub fn play_replay(path: &Path, frame_ms: u64) -> io::Result<()> {
    let text = fs::read_to_string(path)?;
    let frames = split_frames(&text);
    if frames.is_empty() {
        return Ok(());
    }
    if io::stdout().is_terminal() {
        play_interactive(&frames, frame_ms)
    } else {
        play_plain(&frames, frame_ms)
    }
}

fn play_plain(frames: &[String], frame_ms: u64) -> io::Result<()> {
    let mut stdout = io::stdout();
    for frame in frames {
        // Clear + home, then the frame (plain ANSI; no raw mode needed).
        write!(stdout, "\x1b[2J\x1b[H{frame}")?;
        stdout.flush()?;
        thread::sleep(Duration::from_millis(frame_ms));
    }
    Ok(())
}

/// Raw mode + alternate screen for the interactive player; restored on Drop
/// via the same idempotent path the TUI uses.
struct RawScreen;

impl RawScreen {
    fn new() -> io::Result<Self> {
        enable_raw_mode()?;
        if let Err(err) = execute!(io::stdout(), EnterAlternateScreen, Hide) {
            restore_terminal();
            return Err(err);
        }
        Ok(Self)
    }
}

impl Drop for RawScreen {
    fn drop(&mut self) {
        restore_terminal();
    }
}

enum PlayCmd {
    /// Cadence tick: move forward without pausing.
    Advance,
    /// Manual step (pauses playback).
    Step(isize),
    First,
    Last,
    Toggle,
    Redraw,
    Quit,
}

fn play_interactive(frames: &[String], frame_ms: u64) -> io::Result<()> {
    let _guard = RawScreen::new()?;
    let color = env::var_os("NO_COLOR").is_none();
    let last = frames.len() - 1;
    let mut idx = 0;
    let mut playing = frames.len() > 1;
    loop {
        draw_frame(frames, idx, playing, frame_ms, color)?;
        let cmd = if playing {
            read_cmd(Some(Duration::from_millis(frame_ms.max(1))))?
        } else {
            read_cmd(None)?
        };
        match cmd {
            PlayCmd::Quit => return Ok(()),
            PlayCmd::Toggle => playing = !playing,
            PlayCmd::Redraw => {}
            PlayCmd::Advance => {
                if idx < last {
                    idx += 1;
                } else {
                    playing = false;
                }
            }
            PlayCmd::Step(delta) => {
                playing = false;
                idx = idx.saturating_add_signed(delta).min(last);
            }
            PlayCmd::First => {
                playing = false;
                idx = 0;
            }
            PlayCmd::Last => {
                playing = false;
                idx = last;
            }
        }
    }
}

/// Waits for the next player command; `None` blocks until input, `Some`
/// yields [`PlayCmd::Advance`] when the cadence elapses first.
fn read_cmd(timeout: Option<Duration>) -> io::Result<PlayCmd> {
    loop {
        if let Some(timeout) = timeout {
            if !event::poll(timeout)? {
                return Ok(PlayCmd::Advance);
            }
        }
        match event::read()? {
            Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                if let Some(cmd) = key_cmd(key) {
                    return Ok(cmd);
                }
            }
            Event::Resize(..) => return Ok(PlayCmd::Redraw),
            _ => {}
        }
    }
}

fn key_cmd(key: KeyEvent) -> Option<PlayCmd> {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Some(PlayCmd::Quit);
    }
    match key.code {
        KeyCode::Right | KeyCode::Down | KeyCode::Enter | KeyCode::Char('n') | KeyCode::Char('l') | KeyCode::Char('j') => {
            Some(PlayCmd::Step(1))
        }
        KeyCode::Left | KeyCode::Up | KeyCode::Char('p') | KeyCode::Char('h') | KeyCode::Char('k') => Some(PlayCmd::Step(-1)),
        KeyCode::PageDown => Some(PlayCmd::Step(10)),
        KeyCode::PageUp => Some(PlayCmd::Step(-10)),
        KeyCode::Home | KeyCode::Char('g') => Some(PlayCmd::First),
        KeyCode::End | KeyCode::Char('G') => Some(PlayCmd::Last),
        KeyCode::Char(' ') => Some(PlayCmd::Toggle),
        KeyCode::Char('q') | KeyCode::Esc => Some(PlayCmd::Quit),
        _ => None,
    }
}

fn draw_frame(frames: &[String], idx: usize, playing: bool, frame_ms: u64, color: bool) -> io::Result<()> {
    let mut out = String::from("\x1b[2J\x1b[H");
    for line in frames[idx].lines() {
        if color {
            out.push_str(&colorize_line(line));
        } else {
            out.push_str(line);
        }
        // Raw mode: a bare \n no longer implies carriage return.
        out.push_str("\r\n");
    }
    let status = if playing {
        format!("playing {frame_ms}ms/frame")
    } else {
        "paused".to_owned()
    };
    let footer = format!(
        "frame {}/{} · {status} · space play/pause · ←/→ step · Home/End · q quit",
        idx + 1,
        frames.len()
    );
    if color {
        let _ = write!(out, "{DIM}{footer}{RESET}");
    } else {
        out.push_str(&footer);
    }
    let mut stdout = io::stdout();
    stdout.write_all(out.as_bytes())?;
    stdout.flush()
}

/// Playback-time colour, keyed off the line shapes `render_replay` emits.
/// The file on disk stays plain so it greps and diffs cleanly.
fn colorize_line(line: &str) -> String {
    let paint = |code: &str| format!("{code}{line}{RESET}");
    if line.starts_with(FRAME_MARK) {
        paint(BOLD_YELLOW)
    } else if line.starts_with("→ CALL") {
        paint(BOLD_GREEN)
    } else if line.starts_with('✓') {
        paint(CYAN)
    } else if line.contains("WARNING") {
        paint(BOLD_RED)
    } else if let Some(bar) = color_bar(line) {
        bar
    } else if ["  over:", "  ┌", "  ├", "  └", "  ─"].iter().any(|p| line.starts_with(p)) {
        paint(DIM)
    } else {
        line.to_owned()
    }
}

/// Greens the contiguous filled run of a progress bar, if the line has one.
fn color_bar(line: &str) -> Option<String> {
    let start = line.find('█')?;
    let end = line.rfind('█')? + '█'.len_utf8();
    Some(format!("{}{GREEN}{}{RESET}{}", &line[..start], &line[start..end], &line[end..]))
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
    use super::{colorize_line, generate_replay, render_replay, split_frames};
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
        assert!(text.contains("call policy:"), "{text}");
        assert!(text.contains("critical path"), "{text}");
        // Eight entrants mean multiple candidates at the first call, so the
        // runner-up ("over:") and its decisive term appear.
        assert!(text.contains("  over: "), "{text}");
        assert!(text.contains("decided") || text.contains("dead tie"), "{text}");
        // The zoomed-in neighborhood: R1 players are seeds, and their
        // winners/losers have destinations.
        assert!(text.contains("← seed"), "{text}");
        assert!(text.contains("winner → "), "{text}");
        assert!(text.contains("loser → "), "{text}");
        assert!(text.contains("decision log"), "{text}");
        assert!(text.contains("autoplay:"), "{text}");

        let frames = split_frames(&text);
        // Header + one frame per event (log/summary ride with the last).
        assert_eq!(frames.len(), replay.events.len() + 1);
        assert!(frames[0].contains("scheduler autoplay replay"));
        assert!(frames[1].starts_with('▶'), "{}", frames[1]);
    }

    #[tokio::test]
    async fn duration_noise_changes_the_run_deterministically() {
        let source = FixtureSource::from_synth_spec("de:8").unwrap();
        let (mut config, _) = source.derived_config();
        let smooth = generate_replay(&source, &config, NOW).await.unwrap();

        config.sim.duration_noise = 0.4;
        config.sim.noise_seed = 7;
        let noisy_a = generate_replay(&source, &config, NOW).await.unwrap();
        let noisy_b = generate_replay(&source, &config, NOW).await.unwrap();
        assert_eq!(noisy_a.events, noisy_b.events, "same seed, same run");
        assert_ne!(
            smooth.outcome.overall_finish, noisy_a.outcome.overall_finish,
            "noise perturbs the makespan"
        );

        config.sim.noise_seed = 8;
        let reseeded = generate_replay(&source, &config, NOW).await.unwrap();
        assert_ne!(
            noisy_a.outcome.overall_finish, reseeded.outcome.overall_finish,
            "a different seed is a different run"
        );
    }

    #[test]
    fn colorize_targets_the_expected_line_shapes() {
        assert!(colorize_line("▶ T+4m · CALL").starts_with("\x1b[1;33m"));
        assert!(colorize_line("→ CALL setup 1: A vs B").starts_with("\x1b[1;32m"));
        assert!(colorize_line("✓ de-8 A: P1 wins").starts_with("\x1b[36m"));
        assert!(colorize_line("  WARNING x: starved").starts_with("\x1b[1;31m"));
        assert!(colorize_line("  over: C vs D").starts_with("\x1b[2m"));
        assert!(colorize_line("bracket [██░░] 2/4").contains("\x1b[32m██\x1b[0m"));
        assert_eq!(colorize_line("plain text"), "plain text");
    }
}

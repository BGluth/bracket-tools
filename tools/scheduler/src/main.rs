//! `scheduler` — the TO-desk multi-bracket calling tool.
//!
//! Thin wiring only: parse CLI → load config → preflight → spawn the input
//! thread, poller, and tick under a JoinSet → run the Elm loop (recv →
//! update → apply effects → coalesced draw). All logic lives in the library
//! so tests drive it without a terminal or network.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{bail, Context};
use bracket_tools_scheduler::{
    app::{update, AppState, BracketBootstrap, Msg, NoticeLevel, PollFailure, PollHealth, SimUrgency, UpdateEffects, WriteIntent},
    cli::{build_live_source, resolve_token, Cli},
    conflict::UnixMillis,
    fixture_source::{classify_fixture_error, FixtureSource},
    model::BracketId,
    persist::{load_overlay, load_snapshot, save_overlay, save_snapshot, sibling_with_suffix, Load, Lockfile},
    poller::{classify_provider_error, run_poller, PollerConfig},
    preflight::preflight,
    set_source::SetSource,
    terminal::{install_panic_hook, TerminalGuard},
    ui,
    world::{rollout_rankings, SimSnapshot, ROLLOUT_TOP_K},
    writer::{run_writer, WriterConfig},
    SchedulerConfig,
};
use clap::Parser;
use crossterm::event::{Event, KeyEventKind};
use tokio::{
    sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender},
    task::JoinSet,
};

const PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(20);
/// Overlay save cadence: at most one write per this window while state churns.
const SAVE_DEBOUNCE_MS: i64 = 2000;
const DEFAULT_STATE_FILE: &str = "scheduler-state.json";
const DEFAULT_SNAPSHOT_FILE: &str = "scheduler-snapshot.json";
/// Routine rollout evaluations run at most this often; the decision-point
/// exemption (setup freed) bypasses it.
const SIM_DEBOUNCE_MS: i64 = 5000;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config = SchedulerConfig::load(&cli.config)?;

    if let Some(dir) = cli.simulate.clone() {
        let source = Arc::new(FixtureSource::from_captures(&dir).context("loading --simulate fixtures")?);
        run(cli, config, source, classify_fixture_error).await
    } else {
        let token = resolve_token(cli.token.as_deref(), &config)?;
        let source = Arc::new(build_live_source(token)?);
        run(cli, config, source, classify_provider_error).await
    }
}

async fn run<S, F>(cli: Cli, mut config: SchedulerConfig, source: Arc<S>, classify: F) -> anyhow::Result<()>
where
    S: SetSource + Send + Sync + 'static,
    F: Fn(&S::Error) -> PollFailure + Send + Sync + Clone + 'static,
{
    // Single-instance guard, held for the process lifetime. A simulate run
    // uses sibling paths so a rehearsal never touches (or races) live state.
    let state_path = persisted_path(config.state_file.as_deref(), DEFAULT_STATE_FILE, cli.simulate.is_some());
    let snapshot_path = persisted_path(config.snapshot_file.as_deref(), DEFAULT_SNAPSHOT_FILE, cli.simulate.is_some());
    let _lock = Lockfile::acquire(&sibling_with_suffix(&state_path, "lock"))?;

    let arm_writes = !(cli.advisor_only || config.advisor_only);
    let report = preflight(&*source, &config, PREFLIGHT_TIMEOUT, arm_writes, classify.clone()).await;
    print!("{}", report.render());
    if report.fatal.is_some() {
        bail!("preflight failed");
    }
    if cli.preflight_only {
        return Ok(());
    }
    // Advisor-only with no pinned CALLED int: remote-call detection is blind,
    // so deviations escalate to soft-busy (rev-4 gap closure; the report
    // printed the warning).
    if report.escalate_soft_busy {
        config.escalate_unpinned_state_deviation = true;
    }

    let writes_armed = report.writes_armed;
    let mut bootstraps = report.into_bootstraps();
    // Events preflight couldn't fetch open on the persisted last-good
    // snapshot (stale-flagged) instead of a blank table.
    let seeded = seed_from_snapshot(&mut bootstraps, &snapshot_path);
    let events: Vec<BracketId> = bootstraps.iter().map(|b| b.id.clone()).collect();
    let mut state = AppState::new(config.clone(), writes_armed, bootstraps, now_millis());
    restore_overlay(&mut state, &state_path);
    mark_seeded_stale(&mut state, &seeded);

    let (tx, rx) = unbounded_channel::<Msg>();
    let mut tasks = Tasks::new(source, PollerConfig::from_scheduler(&config), events, classify, tx.clone());
    spawn_input_thread(tx.clone());

    install_panic_hook();
    let mut guard = TerminalGuard::new().context("entering the terminal")?;

    guard.terminal.draw(|frame| ui::draw(frame, &state, now_millis()))?;
    event_loop(&mut state, rx, &mut tasks, &mut guard, &state_path, &snapshot_path).await?;

    drop(guard);
    tasks.join_set.abort_all();
    Ok(())
}

/// Where a persisted document lives: the configured path, else the default
/// beside the working directory. Simulate runs get a `.sim` sibling.
fn persisted_path(configured: Option<&Path>, default: &str, simulate: bool) -> PathBuf {
    let base = configured.map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from(default));
    if simulate {
        sibling_with_suffix(&base, "sim")
    } else {
        base
    }
}

/// Fills fetch-failed bootstraps from the persisted last-good snapshot.
/// Returns what was seeded, with each table's capture time (for staleness).
fn seed_from_snapshot(bootstraps: &mut [BracketBootstrap], path: &Path) -> Vec<(BracketId, UnixMillis)> {
    let doc = match load_snapshot(path) {
        Ok(Load::Loaded(doc)) => doc,
        // A corrupt snapshot is just a lost cache; overlay recovery already
        // warns loudly, so start cold quietly.
        Ok(Load::Recovered(_)) | Ok(Load::None) | Err(_) => return Vec::new(),
    };
    let mut seeded = Vec::new();
    for boot in bootstraps.iter_mut() {
        if !boot.sets.is_empty() {
            continue;
        }
        let Some(snap) = doc.brackets.iter().find(|b| b.id == boot.id && !b.sets.is_empty()) else {
            continue;
        };
        boot.sets = snap.sets.clone();
        if boot.groups.is_empty() {
            boot.groups = snap.groups.clone();
        }
        seeded.push((boot.id.clone(), snap.captured_at));
    }
    seeded
}

/// Stamps snapshot-seeded brackets with their true capture age (the staleness
/// badge must not read "fresh") and says so.
fn mark_seeded_stale(state: &mut AppState, seeded: &[(BracketId, UnixMillis)]) {
    let now = now_millis();
    for (id, captured_at) in seeded {
        if let Some(runtime) = state.brackets.iter_mut().find(|b| &b.state.id == id) {
            runtime.last_good_poll = (*captured_at > 0).then_some(*captured_at);
            runtime.health = PollHealth::Offline;
        }
        let age_secs = (now - captured_at) / 1000;
        let text = format!("{}: seeded from the snapshot file ({}m old) — poller retries", id.0, age_secs / 60);
        state.notice(now, NoticeLevel::Warn, text);
    }
}

/// Rehydrates a persisted overlay (if any) over the freshly-bootstrapped
/// state. Corruption recovers to `.bak`; only an unreadable file (permissions)
/// leaves the badge up. Never fails startup.
fn restore_overlay(state: &mut AppState, path: &Path) {
    let now = now_millis();
    match load_overlay(path) {
        Ok(Load::Loaded(doc)) => {
            state.apply_overlay(*doc, now);
            state.notice(now, NoticeLevel::Info, format!("restored session state from {}", path.display()));
        }
        Ok(Load::Recovered(backup)) => {
            let text = format!(
                "state file was corrupt or from another version; backed up to {} — starting fresh",
                backup.display()
            );
            state.notice(now, NoticeLevel::Warn, text);
        }
        Ok(Load::None) => {}
        Err(e) => {
            state.persist_failed = true;
            state.notice(now, NoticeLevel::Error, format!("cannot read state file: {e}"));
        }
    }
}

/// One overlay save, tracking the badge through failure and recovery.
fn persist_overlay(state: &mut AppState, path: &Path) {
    let result = save_overlay(path, &state.to_overlay());
    track_persist_outcome(state, result.err().map(|e| e.to_string()));
    state.overlay_dirty = false;
}

/// One snapshot-file save; shares the badge with the overlay.
fn persist_snapshot(state: &mut AppState, path: &Path) {
    let result = save_snapshot(path, &state.to_snapshot());
    track_persist_outcome(state, result.err().map(|e| e.to_string()));
    state.snapshot_dirty = false;
}

fn track_persist_outcome(state: &mut AppState, error: Option<String>) {
    match error {
        None => {
            if state.persist_failed {
                state.notice(now_millis(), NoticeLevel::Info, "state files writable again");
            }
            state.persist_failed = false;
        }
        Some(e) => {
            if !state.persist_failed {
                state.notice(now_millis(), NoticeLevel::Error, format!("state save failed: {e}"));
            }
            state.persist_failed = true;
        }
    }
}

async fn event_loop<S, F>(
    state: &mut AppState,
    mut rx: UnboundedReceiver<Msg>,
    tasks: &mut Tasks<S, F>,
    guard: &mut TerminalGuard,
    state_path: &Path,
    snapshot_path: &Path,
) -> anyhow::Result<()>
where
    S: SetSource + Send + Sync + 'static,
    F: Fn(&S::Error) -> PollFailure + Send + Sync + Clone + 'static,
{
    let mut last_save: UnixMillis = 0;
    let mut last_sim_dispatch: UnixMillis = 0;
    let mut sim_pending = false;
    loop {
        let mut effects = UpdateEffects::default();
        tokio::select! {
            maybe_msg = rx.recv() => {
                let Some(msg) = maybe_msg else { break };
                effects = update(state, msg, now_millis());
                // Coalesce whatever else is already queued before drawing.
                while let Ok(more) = rx.try_recv() {
                    effects.merge(update(state, more, now_millis()));
                }
            }
            died = tasks.join_set.join_next() => {
                if let Some(result) = died {
                    let name = tasks.restart_dead_task(result);
                    state.notice(now_millis(), NoticeLevel::Error, format!("internal task died and was restarted: {name}"));
                    state.dirty = true;
                }
            }
        }

        for bracket in effects.force_poll.drain(..) {
            let _ = tasks.force_tx.send(bracket);
        }
        for intent in effects.writes.drain(..) {
            let _ = tasks.write_tx.send(intent);
        }
        if effects.quit {
            break;
        }
        // Rollout triggers: Immediate (setup freed) bypasses the debounce;
        // Routine coalesces to one evaluation per window (the 1s tick keeps
        // this loop spinning, so pending requests flush on time).
        if effects.sim == Some(SimUrgency::Routine) {
            sim_pending = true;
        }
        let now = now_millis();
        if effects.sim == Some(SimUrgency::Immediate) || (sim_pending && now - last_sim_dispatch >= SIM_DEBOUNCE_MS) {
            let _ = tasks.sim_tx.send(state.sim_snapshot(now));
            last_sim_dispatch = now;
            sim_pending = false;
        }
        if (state.overlay_dirty || state.snapshot_dirty) && now_millis() - last_save >= SAVE_DEBOUNCE_MS {
            if state.overlay_dirty {
                persist_overlay(state, state_path);
            }
            if state.snapshot_dirty {
                persist_snapshot(state, snapshot_path);
            }
            last_save = now_millis();
        }
        guard.terminal.draw(|frame| ui::draw(frame, state, now_millis()))?;
    }

    // Final flush so a clean quit never loses the debounce window.
    if state.overlay_dirty {
        persist_overlay(state, state_path);
    }
    if state.snapshot_dirty {
        persist_snapshot(state, snapshot_path);
    }
    Ok(())
}

/// The supervised background tasks plus everything needed to respawn them.
struct Tasks<S, F>
where
    S: SetSource + Send + Sync + 'static,
    F: Fn(&S::Error) -> PollFailure + Send + Sync + Clone + 'static,
{
    join_set: JoinSet<&'static str>,
    force_tx: UnboundedSender<BracketId>,
    write_tx: UnboundedSender<WriteIntent>,
    sim_tx: UnboundedSender<SimSnapshot>,
    source: Arc<S>,
    poller_config: PollerConfig,
    events: Vec<BracketId>,
    classify: F,
    tx: UnboundedSender<Msg>,
}

impl<S, F> Tasks<S, F>
where
    S: SetSource + Send + Sync + 'static,
    F: Fn(&S::Error) -> PollFailure + Send + Sync + Clone + 'static,
{
    fn new(source: Arc<S>, poller_config: PollerConfig, events: Vec<BracketId>, classify: F, tx: UnboundedSender<Msg>) -> Self {
        let (force_tx, _unused_force) = unbounded_channel();
        let (write_tx, _unused_write) = unbounded_channel();
        let (sim_tx, _unused_sim) = unbounded_channel();
        let mut tasks = Self {
            join_set: JoinSet::new(),
            force_tx,
            write_tx,
            sim_tx,
            source,
            poller_config,
            events,
            classify,
            tx,
        };
        tasks.spawn_poller();
        tasks.spawn_writer();
        tasks.spawn_tick();
        tasks.spawn_simulator();
        tasks
    }

    fn spawn_poller(&mut self) {
        let (force_tx, force_rx) = unbounded_channel();
        self.force_tx = force_tx;
        let (source, events, config, classify, tx) = (
            self.source.clone(),
            self.events.clone(),
            self.poller_config.clone(),
            self.classify.clone(),
            self.tx.clone(),
        );
        self.join_set.spawn(async move {
            run_poller(&*source, events, config, classify, tx, force_rx).await;
            "poller"
        });
    }

    fn spawn_writer(&mut self) {
        let (write_tx, write_rx) = unbounded_channel();
        self.write_tx = write_tx;
        let (source, classify, tx) = (self.source.clone(), self.classify.clone(), self.tx.clone());
        self.join_set.spawn(async move {
            run_writer(&*source, WriterConfig::default(), classify, tx, write_rx).await;
            "writer"
        });
    }

    fn spawn_tick(&mut self) {
        let tx = self.tx.clone();
        self.join_set.spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                if tx.send(Msg::Tick).is_err() {
                    return "tick";
                }
            }
        });
    }

    /// The background rollout evaluator: drains to the newest snapshot (only
    /// the latest world matters), evaluates on a blocking thread (N+1 forward
    /// simulations), and publishes the ranking as a [`Msg::SimResult`].
    fn spawn_simulator(&mut self) {
        let (sim_tx, mut sim_rx) = unbounded_channel::<SimSnapshot>();
        self.sim_tx = sim_tx;
        let tx = self.tx.clone();
        self.join_set.spawn(async move {
            while let Some(mut snapshot) = sim_rx.recv().await {
                while let Ok(newer) = sim_rx.try_recv() {
                    snapshot = newer;
                }
                let Ok(rankings) = tokio::task::spawn_blocking(move || rollout_rankings(&snapshot, ROLLOUT_TOP_K)).await else {
                    return "simulator";
                };
                if tx.send(Msg::SimResult(rankings)).is_err() {
                    return "simulator";
                }
            }
            "simulator"
        });
    }

    /// Respawns whichever supervised task ended; returns its name for the
    /// banner. A panic (Err) can't name its task; the poller is the likely
    /// culprit (the tick task's only fallible operation is a channel send
    /// that returns cleanly), so panics respawn it. In-flight write intents
    /// die with a crashed writer; the app's pending list still shows them.
    fn restart_dead_task(&mut self, result: Result<&'static str, tokio::task::JoinError>) -> &'static str {
        let name = result.unwrap_or("poller (panicked)");
        match name {
            "tick" => self.spawn_tick(),
            "writer" => self.spawn_writer(),
            "simulator" => self.spawn_simulator(),
            _ => self.spawn_poller(),
        }
        name
    }
}

/// Blocking input thread: crossterm reads forwarded into the Elm mailbox.
/// (`UnboundedSender::send` is sync, so no async runtime is needed here.)
fn spawn_input_thread(tx: UnboundedSender<Msg>) {
    thread::spawn(move || loop {
        match crossterm::event::read() {
            Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                if tx.send(Msg::Key(key)).is_err() {
                    return;
                }
            }
            Ok(_) => {}
            Err(_) => return,
        }
    });
}

fn now_millis() -> UnixMillis {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

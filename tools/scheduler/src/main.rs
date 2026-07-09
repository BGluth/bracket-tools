//! `scheduler` — the TO-desk multi-bracket calling tool.
//!
//! Thin wiring only: parse CLI → load config → preflight → spawn the input
//! thread, poller, and tick under a JoinSet → run the Elm loop (recv →
//! update → apply effects → coalesced draw). All logic lives in the library
//! so tests drive it without a terminal or network.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{bail, Context};
use bracket_tools_scheduler::{
    app::{update, AppState, BracketBootstrap, Msg, NoticeLevel, PollFailure, PollHealth, SimUrgency, UpdateEffects, WriteIntent},
    cli::{build_live_source, default_data_dir, resolve_token, Cli},
    config::{referenced_types, resolve_roster, write_starter_template, SetupCounts},
    conflict::UnixMillis,
    fixture_source::{classify_fixture_error, FixtureSource},
    model::BracketId,
    persist::{
        load_overlay, load_setup_defaults, load_snapshot, save_overlay, save_setup_defaults, save_snapshot, sibling_with_suffix, Load,
        Lockfile,
    },
    poller::{classify_provider_error, run_poller, PollerConfig},
    preflight::preflight,
    rehearsal::install_rehearsal,
    replay::{generate_replay, play_replay, render_replay},
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
/// Cross-tournament setup-count defaults, in the XDG data dir.
const SETUP_DEFAULTS_FILE: &str = "setup-defaults.toml";
/// Routine rollout evaluations run at most this often; the decision-point
/// exemption (setup freed) bypasses it.
const SIM_DEBOUNCE_MS: i64 = 5000;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    if let Some(path) = &cli.replay {
        return play_replay(path, cli.frame_ms).with_context(|| format!("playing {}", path.display()));
    }
    let config_path = cli.config_path();

    if cli.offline() {
        let mut source = build_offline_source(&cli)?;
        let mut config = offline_config(&cli, &config_path, &source)?;
        apply_noise_overrides(&cli, &mut config)?;
        resolve_setup_counts(&cli, &mut config)?;
        if cli.autoplay {
            return autoplay(&cli, &config, &source).await;
        }
        if let Some(speed) = cli.pace {
            let report = install_rehearsal(&mut source, &config, speed, now_millis())
                .await
                .context("building the --pace rehearsal")?;
            print!("{}", report.render());
        }
        run(cli, config, Arc::new(source), classify_fixture_error).await
    } else {
        let Some(mut config) = SchedulerConfig::load_if_present(&config_path)? else {
            return bootstrap_starter_config(&config_path);
        };
        resolve_setup_counts(&cli, &mut config)?;
        let token = resolve_token(cli.token.as_deref(), &config)?;
        let source = Arc::new(build_live_source(token)?);
        run(cli, config, source, classify_provider_error).await
    }
}

fn build_offline_source(cli: &Cli) -> anyhow::Result<FixtureSource> {
    if let Some(dir) = &cli.simulate {
        FixtureSource::from_captures(dir).context("loading --simulate fixtures")
    } else {
        let spec = cli.synth.as_deref().expect("offline() implies a synth spec");
        FixtureSource::from_synth_spec(spec).context("building the --synth world")
    }
}

/// Picks the offline run's config. A *discovered* config (no `--config`
/// flag) whose brackets name no event in the offline world is ignored in
/// favor of a derived one — the live starter template or a real tournament's
/// config would otherwise fail every preflight against a world that cannot
/// contain its slugs. An explicit `--config` is always honored.
fn offline_config(cli: &Cli, config_path: &Path, source: &FixtureSource) -> anyhow::Result<SchedulerConfig> {
    match SchedulerConfig::load_if_present(config_path)? {
        Some(config) if cli.config.is_some() || config.brackets.iter().any(|b| source.has_event(&b.slug)) => Ok(config),
        Some(_) => {
            println!(
                "config at {} names no event in this offline world — ignoring it (pass --config to force it)",
                config_path.display()
            );
            Ok(derive_offline_config(source))
        }
        None => {
            println!("no config at {}", config_path.display());
            Ok(derive_offline_config(source))
        }
    }
}

/// Folds the remaining count sources into `config.setups` before the app is
/// built: `--setups` beats the config, the config beats the defaults file,
/// and a still-`None` result reaches the constructor's fallback-4 warning.
/// (The persisted overlay's roster slots in *between* the flag and the
/// config — `apply_overlay` adopts it unless the flag pinned counts.)
fn resolve_setup_counts(cli: &Cli, config: &mut SchedulerConfig) -> anyhow::Result<()> {
    resolve_setup_counts_from(cli, config, &default_data_dir().join(SETUP_DEFAULTS_FILE))
}

fn resolve_setup_counts_from(cli: &Cli, config: &mut SchedulerConfig, defaults_path: &Path) -> anyhow::Result<()> {
    if let Some(counts) = &cli.setups {
        config.setups = Some(counts.clone());
    } else if config.setups.is_none() {
        if let Some(table) = load_setup_defaults(defaults_path) {
            // Only types this config's brackets reference: a stale entry from
            // another venue must not materialize ghost stations.
            let referenced = referenced_types(config);
            let table: BTreeMap<String, u32> = table.into_iter().filter(|(t, _)| referenced.contains(t)).collect();
            if !table.is_empty() {
                config.setups = Some(SetupCounts::ByType(table));
            }
        }
    }
    config.validate().context("applying --setups")?;
    Ok(())
}

/// `--noise`/`--noise-seed` beat whatever the config's `[sim]` section says;
/// re-validation keeps the fraction inside the sane band.
fn apply_noise_overrides(cli: &Cli, config: &mut SchedulerConfig) -> anyhow::Result<()> {
    if cli.noise.is_none() && cli.noise_seed.is_none() {
        return Ok(());
    }
    if let Some(noise) = cli.noise {
        config.sim.duration_noise = noise;
    }
    if let Some(seed) = cli.noise_seed {
        config.sim.noise_seed = seed;
    }
    config.validate().context("applying --noise")?;
    Ok(())
}

/// Zero-config offline runs derive their config from the world itself.
fn derive_offline_config(source: &FixtureSource) -> SchedulerConfig {
    let (config, skipped) = source.derived_config();
    println!(
        "derived config: {} event(s), {} shared setups, writes fixture-armed",
        config.brackets.len(),
        resolve_roster(&config).roster.len(),
    );
    if !skipped.is_empty() {
        println!("  skipped (a different tournament than the largest): {}", skipped.join(", "));
    }
    println!("  write a config file to pin real setups and pools");
    config
}

/// `--autoplay`: the sim plays the whole offline world, the replay lands in
/// a file, and the decision summary prints — no TUI, no lockfile.
async fn autoplay(cli: &Cli, config: &SchedulerConfig, source: &FixtureSource) -> anyhow::Result<()> {
    let replay = generate_replay(source, config, now_millis())
        .await
        .context("generating the autoplay replay")?;
    let text = render_replay(&replay);
    fs::write(&cli.replay_out, &text).with_context(|| format!("writing {}", cli.replay_out.display()))?;
    print!("{}", replay.summary());
    println!(
        "replay written to {} — watch it: scheduler --replay {}",
        cli.replay_out.display(),
        cli.replay_out.display()
    );
    Ok(())
}

/// No config in live mode: write the commented starter and exit so the user
/// reviews it before the tool ever talks to start.gg.
fn bootstrap_starter_config(path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    write_starter_template(path).with_context(|| format!("writing the starter config to {}", path.display()))?;
    println!(
        "No config found — created a starter at {}.\n\
         Fill in your tournament's events and setups, then rerun.\n\
         (Tip: --simulate <captures-dir> and --synth de:32,rr:8 run with no config at all.)",
        path.display()
    );
    Ok(())
}

async fn run<S, F>(cli: Cli, mut config: SchedulerConfig, source: Arc<S>, classify: F) -> anyhow::Result<()>
where
    S: SetSource + Send + Sync + 'static,
    F: Fn(&S::Error) -> PollFailure + Send + Sync + Clone + 'static,
{
    // Single-instance guard, held for the process lifetime. An offline run
    // uses sibling paths so a rehearsal never touches (or races) live state.
    let state_path = persisted_path(config.state_file.as_deref(), DEFAULT_STATE_FILE, cli.offline());
    let snapshot_path = persisted_path(config.snapshot_file.as_deref(), DEFAULT_SNAPSHOT_FILE, cli.offline());
    if let Some(parent) = state_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
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

    // Live roster changes reseed the cross-tournament defaults; offline
    // (synth/rehearsal) worlds must not stomp the venue's real counts.
    let defaults_path = (!cli.offline()).then(|| default_data_dir().join(SETUP_DEFAULTS_FILE));

    guard.terminal.draw(|frame| ui::draw(frame, &state, now_millis()))?;
    event_loop(
        &mut state,
        rx,
        &mut tasks,
        &mut guard,
        &state_path,
        &snapshot_path,
        defaults_path.as_deref(),
    )
    .await?;

    drop(guard);
    tasks.join_set.abort_all();
    Ok(())
}

/// Where a persisted document lives: the configured path, else the default
/// name under the XDG data dir. Offline runs get a `.sim` sibling.
fn persisted_path(configured: Option<&Path>, default: &str, offline: bool) -> PathBuf {
    let base = configured
        .map(Path::to_path_buf)
        .unwrap_or_else(|| default_data_dir().join(default));
    if offline {
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
    defaults_path: Option<&Path>,
) -> anyhow::Result<()>
where
    S: SetSource + Send + Sync + 'static,
    F: Fn(&S::Error) -> PollFailure + Send + Sync + Clone + 'static,
{
    let mut last_save: UnixMillis = 0;
    let mut last_sim_dispatch: UnixMillis = 0;
    let mut sim_pending = false;
    // Defaults rewrite only on a roster *change*, so a session that never
    // touches 's' leaves the file alone.
    let mut last_counts = state.board.counts_by_type();
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
            persist_setup_defaults(state, defaults_path, &mut last_counts);
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
    persist_setup_defaults(state, defaults_path, &mut last_counts);
    Ok(())
}

/// Rewrites the cross-tournament defaults file when the roster shape changed
/// (live sessions only — `defaults_path` is `None` offline). Best-effort: the
/// overlay save already tracks the persistence badge.
fn persist_setup_defaults(state: &AppState, path: Option<&Path>, last_counts: &mut BTreeMap<String, u32>) {
    let Some(path) = path else { return };
    let counts = state.board.counts_by_type();
    if counts != *last_counts {
        let _ = save_setup_defaults(path, &counts);
        *last_counts = counts;
    }
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

#[cfg(test)]
mod tests {
    use std::{env, fs, path::PathBuf, process};

    use std::collections::BTreeMap;

    use bracket_tools_scheduler::{
        config::{BracketConfig, OneOrMany, SchedulerConfig, SetupCounts},
        fixture_source::FixtureSource,
        persist::save_setup_defaults,
    };
    use clap::Parser;

    use crate::{offline_config, resolve_setup_counts_from, Cli};

    const WORLD_SLUG: &str = "tournament/synth/event/de8-1";

    fn synth_source() -> FixtureSource {
        FixtureSource::from_synth_spec("de:8").unwrap()
    }

    fn synth_cli(extra: &[&str]) -> Cli {
        let mut argv = vec!["scheduler", "--synth", "de:8"];
        argv.extend_from_slice(extra);
        Cli::try_parse_from(argv).unwrap()
    }

    fn write_config(name: &str, slug: &str) -> PathBuf {
        let path = env::temp_dir().join(format!("scheduler-main-test-{}-{name}.toml", process::id()));
        fs::write(&path, format!("setups = 2\n\n[[brackets]]\nslug = {slug:?}\n")).unwrap();
        path
    }

    #[test]
    fn missing_config_derives_from_the_world() {
        let path = env::temp_dir().join(format!("scheduler-main-test-{}-absent.toml", process::id()));
        let config = offline_config(&synth_cli(&[]), &path, &synth_source()).unwrap();
        assert_eq!(config.brackets[0].slug, WORLD_SLUG);
    }

    #[test]
    fn discovered_config_outside_the_world_is_ignored() {
        // The live starter template's placeholder slugs can never exist in a
        // synth or capture world; discovering one must not poison the run.
        let path = write_config("starter", "tournament/your-tournament/event/your-main-event");
        let config = offline_config(&synth_cli(&[]), &path, &synth_source()).unwrap();
        assert_eq!(config.brackets[0].slug, WORLD_SLUG);
    }

    #[test]
    fn discovered_config_naming_a_world_event_is_honored() {
        let path = write_config("matching", WORLD_SLUG);
        let config = offline_config(&synth_cli(&[]), &path, &synth_source()).unwrap();
        assert_eq!(
            config.setups,
            Some(SetupCounts::Uniform(2)),
            "the discovered config itself must be used, not a derived one"
        );
    }

    #[test]
    fn explicit_config_is_honored_even_outside_the_world() {
        let path = write_config("explicit", "tournament/real/event/main");
        let cli = synth_cli(&["--config", path.to_str().unwrap()]);
        let config = offline_config(&cli, &path, &synth_source()).unwrap();
        assert_eq!(config.brackets[0].slug, "tournament/real/event/main");
    }

    fn switch_config(setups: Option<SetupCounts>) -> SchedulerConfig {
        SchedulerConfig {
            brackets: vec![BracketConfig {
                setup_type: Some(OneOrMany::One("switch".to_owned())),
                ..BracketConfig::new("tournament/t/event/melee")
            }],
            setups,
            ..SchedulerConfig::default()
        }
    }

    #[test]
    fn setup_counts_precedence_flag_config_defaults_fallback() {
        let dir = env::temp_dir().join(format!("scheduler-defaults-test-{}", process::id()));
        fs::create_dir_all(&dir).unwrap();
        let defaults = dir.join("setup-defaults.toml");
        save_setup_defaults(&defaults, &BTreeMap::from([("switch".to_owned(), 5), ("stale".to_owned(), 3)])).unwrap();

        // The flag beats everything.
        let cli = Cli::try_parse_from(["scheduler", "--setups", "switch=2"]).unwrap();
        let mut config = switch_config(Some(SetupCounts::Uniform(9)));
        config.brackets[0].setup_type = None;
        resolve_setup_counts_from(&cli, &mut config, &defaults).unwrap();
        assert_eq!(config.setups, Some(SetupCounts::ByType(BTreeMap::from([("switch".to_owned(), 2)]))));

        // Config counts beat the defaults file.
        let cli = Cli::try_parse_from(["scheduler"]).unwrap();
        let mut config = switch_config(Some(SetupCounts::ByType(BTreeMap::from([("switch".to_owned(), 7)]))));
        resolve_setup_counts_from(&cli, &mut config, &defaults).unwrap();
        assert_eq!(config.setups, Some(SetupCounts::ByType(BTreeMap::from([("switch".to_owned(), 7)]))));

        // No flag, no config counts: the defaults file seeds referenced types
        // only (the stale venue entry must not materialize ghost stations).
        let mut config = switch_config(None);
        resolve_setup_counts_from(&cli, &mut config, &defaults).unwrap();
        assert_eq!(config.setups, Some(SetupCounts::ByType(BTreeMap::from([("switch".to_owned(), 5)]))));

        // Nothing anywhere: stays None for the constructor's fallback-4 warn.
        let mut config = switch_config(None);
        resolve_setup_counts_from(&cli, &mut config, &dir.join("absent.toml")).unwrap();
        assert_eq!(config.setups, None);

        // A defaults file with no referenced type is ignored entirely.
        save_setup_defaults(&defaults, &BTreeMap::from([("other".to_owned(), 4)])).unwrap();
        let mut config = switch_config(None);
        resolve_setup_counts_from(&cli, &mut config, &defaults).unwrap();
        assert_eq!(config.setups, None);

        // A flag that contradicts the bracket types fails validation.
        let cli = Cli::try_parse_from(["scheduler", "--setups", "8"]).unwrap();
        let mut config = switch_config(None);
        assert!(resolve_setup_counts_from(&cli, &mut config, &defaults).is_err());

        fs::remove_dir_all(&dir).unwrap();
    }
}

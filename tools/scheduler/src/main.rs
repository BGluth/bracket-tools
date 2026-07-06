//! `scheduler` — the TO-desk multi-bracket calling tool.
//!
//! Thin wiring only: parse CLI → load config → preflight → spawn the input
//! thread, poller, and tick under a JoinSet → run the Elm loop (recv →
//! update → apply effects → coalesced draw). All logic lives in the library
//! so tests drive it without a terminal or network.

use std::{
    sync::Arc,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{bail, Context};
use bracket_tools_scheduler::{
    app::{update, AppState, Msg, NoticeLevel, PollFailure, UpdateEffects, WriteIntent},
    cli::{build_live_source, resolve_token, Cli},
    conflict::UnixMillis,
    fixture_source::{classify_fixture_error, FixtureSource},
    model::BracketId,
    poller::{classify_provider_error, run_poller, PollerConfig},
    preflight::preflight,
    set_source::SetSource,
    terminal::{install_panic_hook, TerminalGuard},
    ui,
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

async fn run<S, F>(cli: Cli, config: SchedulerConfig, source: Arc<S>, classify: F) -> anyhow::Result<()>
where
    S: SetSource + Send + Sync + 'static,
    F: Fn(&S::Error) -> PollFailure + Send + Sync + Clone + 'static,
{
    let arm_writes = !(cli.advisor_only || config.advisor_only);
    let report = preflight(&*source, &config, PREFLIGHT_TIMEOUT, arm_writes, classify.clone()).await;
    print!("{}", report.render());
    if report.fatal.is_some() {
        bail!("preflight failed");
    }
    if cli.preflight_only {
        return Ok(());
    }

    let writes_armed = report.writes_armed;
    let bootstraps = report.into_bootstraps();
    let events: Vec<BracketId> = bootstraps.iter().map(|b| b.id.clone()).collect();
    let mut state = AppState::new(config.clone(), writes_armed, bootstraps, now_millis());

    let (tx, rx) = unbounded_channel::<Msg>();
    let mut tasks = Tasks::new(source, PollerConfig::from_scheduler(&config), events, classify, tx.clone());
    spawn_input_thread(tx.clone());

    install_panic_hook();
    let mut guard = TerminalGuard::new().context("entering the terminal")?;

    guard.terminal.draw(|frame| ui::draw(frame, &state, now_millis()))?;
    event_loop(&mut state, rx, &mut tasks, &mut guard).await?;

    drop(guard);
    tasks.join_set.abort_all();
    Ok(())
}

async fn event_loop<S, F>(
    state: &mut AppState,
    mut rx: UnboundedReceiver<Msg>,
    tasks: &mut Tasks<S, F>,
    guard: &mut TerminalGuard,
) -> anyhow::Result<()>
where
    S: SetSource + Send + Sync + 'static,
    F: Fn(&S::Error) -> PollFailure + Send + Sync + Clone + 'static,
{
    loop {
        let mut effects = UpdateEffects::default();
        tokio::select! {
            maybe_msg = rx.recv() => {
                let Some(msg) = maybe_msg else { return Ok(()) };
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
            return Ok(());
        }
        guard.terminal.draw(|frame| ui::draw(frame, state, now_millis()))?;
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
        let mut tasks = Self {
            join_set: JoinSet::new(),
            force_tx,
            write_tx,
            source,
            poller_config,
            events,
            classify,
            tx,
        };
        tasks.spawn_poller();
        tasks.spawn_writer();
        tasks.spawn_tick();
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

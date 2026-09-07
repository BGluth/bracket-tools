//! The Elm loop over the scheduler core, driven by the platform's task
//! runtime: the poller, writer and tick feed one inbox, one drain task
//! applies every message to the shared state and routes the effects.

use std::{collections::HashSet, rc::Rc, time::Duration};

use bracket_tools_scheduler_core::{
    app::{update, AppState, BracketBootstrap, Msg, PollFailure},
    config::SchedulerConfig,
    model::BracketId,
    poller::{run_poller, PollerConfig},
    set_source::SetSource,
    timers::{now_millis, sleep},
    ui_action::UiAction,
    writer::{run_writer, WriterConfig},
};
use dioxus::prelude::*;
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

/// A running desk: the state every view reads and the inbox every view and
/// background task writes to.
#[derive(Clone, PartialEq)]
pub struct Session {
    pub state: Signal<AppState>,
    inbox: Signal<UnboundedSender<Msg>>,
}

impl Session {
    pub fn dispatch(&self, action: UiAction) {
        let _ = self.inbox.read().send(Msg::Action(action));
    }
}

/// Starts the background tasks over `source` and hands back the session.
/// Must run inside a component or one of its tasks.
pub fn start<S, F>(source: Rc<S>, config: SchedulerConfig, writes_armed: bool, bootstraps: Vec<BracketBootstrap>, classify: F) -> Session
where
    S: SetSource + 'static,
    F: Fn(&S::Error) -> PollFailure + Clone + 'static,
{
    let needs_structure: HashSet<BracketId> = bootstraps.iter().filter(|b| b.groups.is_empty()).map(|b| b.id.clone()).collect();
    let events: Vec<BracketId> = bootstraps.iter().map(|b| b.id.clone()).collect();
    let mut state = Signal::new(AppState::new(config.clone(), writes_armed, bootstraps, now_millis()));
    let (tx, mut rx) = unbounded_channel::<Msg>();
    let (force_tx, force_rx) = unbounded_channel::<BracketId>();
    let (write_tx, write_rx) = unbounded_channel();

    let poller_config = PollerConfig::from_scheduler(&config);
    let poll_source = source.clone();
    let poll_classify = classify.clone();
    let poll_tx = tx.clone();
    spawn(async move {
        run_poller(
            &*poll_source,
            events,
            poller_config,
            poll_classify,
            poll_tx,
            force_rx,
            needs_structure,
        )
        .await;
    });
    let writer_tx = tx.clone();
    spawn(async move {
        run_writer(&*source, WriterConfig::default(), classify, writer_tx, write_rx).await;
    });
    let tick_tx = tx.clone();
    spawn(async move {
        loop {
            sleep(Duration::from_secs(1)).await;
            if tick_tx.send(Msg::Tick).is_err() {
                return;
            }
        }
    });
    spawn(async move {
        while let Some(msg) = rx.recv().await {
            let mut effects = state.with_mut(|s| update(s, msg, now_millis()));
            for bracket in effects.force_poll.drain(..) {
                let _ = force_tx.send(bracket);
            }
            for intent in effects.writes.drain(..) {
                let _ = write_tx.send(intent);
            }
        }
    });
    Session {
        state,
        inbox: Signal::new(tx),
    }
}

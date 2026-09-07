//! The Elm loop over the scheduler core, driven by the platform's task
//! runtime: the poller, writer and tick feed one inbox, one drain task
//! applies every message to the shared state, routes the effects and
//! debounces the saves.

use std::{collections::HashSet, rc::Rc, time::Duration};

use bracket_tools_scheduler_core::{
    app::{update, AppState, Msg, PollFailure},
    conflict::UnixMillis,
    keymap::Key,
    model::BracketId,
    poller::{run_poller, PollerConfig},
    set_source::SetSource,
    timers::{now_millis, sleep},
    ui_action::UiAction,
    writer::{run_writer, WriterConfig},
};
use dioxus::prelude::*;
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

use crate::persist::DeskPersistence;

/// Quiet time after a change before the overlay and snapshot are saved.
const SAVE_DEBOUNCE_MS: UnixMillis = 2000;

/// A running desk: the state every view reads and the inbox every view and
/// background task writes to.
#[derive(Clone, PartialEq)]
pub struct Session {
    pub state: Signal<AppState>,
    inbox: Signal<UnboundedSender<Msg>>,
    persistence: Signal<Option<Rc<DeskPersistence>>>,
}

impl Session {
    pub fn dispatch(&self, action: UiAction) {
        let _ = self.inbox.read().send(Msg::Action(action));
    }

    /// A key from the page, resolved through the core's keymap.
    pub fn send_key(&self, key: Key) {
        let _ = self.inbox.read().send(Msg::Key(key));
    }

    /// Saves whatever is dirty now, ahead of the debounce (the page is
    /// hiding or unloading).
    pub fn flush(&self) {
        if let Some(persistence) = self.persistence.read().as_ref() {
            save_dirty(self.state, persistence);
        }
    }
}

/// Starts the background tasks over `source` for an already-bootstrapped
/// (and possibly restored) `state`, saving through `persistence` when given.
/// Must run inside a component or one of its tasks.
pub fn start<S, F>(source: Rc<S>, state: AppState, classify: F, persistence: Option<Rc<DeskPersistence>>) -> Session
where
    S: SetSource + 'static,
    F: Fn(&S::Error) -> PollFailure + Clone + 'static,
{
    let events: Vec<BracketId> = state.brackets.iter().map(|b| b.state.id.clone()).collect();
    // Brackets whose structure never arrived (preflight blip or a snapshot
    // seed without groups) get it back-filled by the poller.
    let needs_structure: HashSet<BracketId> = state
        .brackets
        .iter()
        .filter(|b| b.state.groups.is_empty())
        .map(|b| b.state.id.clone())
        .collect();
    let poller_config = PollerConfig::from_scheduler(&state.config);
    let mut state = Signal::new(state);
    let (tx, mut rx) = unbounded_channel::<Msg>();
    let (force_tx, force_rx) = unbounded_channel::<BracketId>();
    let (write_tx, write_rx) = unbounded_channel();

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
    let drain_persistence = persistence.clone();
    spawn(async move {
        let mut last_save: UnixMillis = 0;
        while let Some(msg) = rx.recv().await {
            let mut effects = state.with_mut(|s| update(s, msg, now_millis()));
            for bracket in effects.force_poll.drain(..) {
                let _ = force_tx.send(bracket);
            }
            for intent in effects.writes.drain(..) {
                let _ = write_tx.send(intent);
            }
            if let Some(persistence) = &drain_persistence {
                save_if_due(state, persistence, &mut last_save);
            }
        }
    });
    Session {
        state,
        inbox: Signal::new(tx),
        persistence: Signal::new(persistence),
    }
}

/// Saves once the debounce window has passed since the last save.
fn save_if_due(state: Signal<AppState>, persistence: &Rc<DeskPersistence>, last_save: &mut UnixMillis) {
    let now = now_millis();
    let dirty = {
        let s = state.read();
        s.overlay_dirty || s.snapshot_dirty
    };
    if !dirty || now - *last_save < SAVE_DEBOUNCE_MS {
        return;
    }
    *last_save = now;
    save_dirty(state, persistence);
}

/// Saves whatever is dirty; the writes run as their own tasks and report
/// back into the persistence badge.
fn save_dirty(mut state: Signal<AppState>, persistence: &Rc<DeskPersistence>) {
    let (overlay, snapshot) = {
        let s = state.read();
        (s.overlay_dirty, s.snapshot_dirty)
    };
    if overlay {
        let doc = state.with_mut(|s| {
            s.overlay_dirty = false;
            s.to_overlay()
        });
        let persistence = persistence.clone();
        spawn(async move {
            let outcome = persistence.save_overlay(&doc).await.err();
            state.with_mut(|s| s.record_persist_outcome(outcome, now_millis()));
        });
    }
    if snapshot {
        let doc = state.with_mut(|s| {
            s.snapshot_dirty = false;
            s.to_snapshot()
        });
        let persistence = persistence.clone();
        spawn(async move {
            let outcome = persistence.save_snapshot(&doc).await.err();
            state.with_mut(|s| s.record_persist_outcome(outcome, now_millis()));
        });
    }
}

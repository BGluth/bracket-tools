//! A token-free world for trying the desk: a synthetic tournament played
//! forward on a fast clock, served by the fixture source. The desk's state
//! persists like a live one's; "reset" forgets it and boots a fresh world.

use std::{rc::Rc, time::Duration};

use bracket_tools_scheduler_core::{
    app::AppState,
    fixture_source::{classify_fixture_error, FixtureSource},
    preflight::{preflight, PreflightEnv},
    rehearsal::install_rehearsal,
    timers::now_millis,
};
use dioxus::prelude::*;

use crate::{
    bridge::{start, Session},
    persist::{apply_saved, peek_saved, DeskPersistence, DeskStore, SavedState},
    views::Desk,
    DESK_CSS,
};

/// A 32-player double elimination plus an 8-player round robin sharing players.
const SYNTH_SPEC: &str = "de:32,rr:8";
/// Wall-clock speed-up for the scripted completions.
const SPEED: f64 = 20.0;
const PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(10);

/// The scheduler over the demo world. Each reset remounts the desk under a
/// new key, which drops the previous world's tasks with its scope.
#[component]
pub fn DemoTool() -> Element {
    let mut generation = use_signal(|| 0u32);
    rsx! {
        DemoDesk {
            key: "{generation}",
            on_reset: move |_| {
                spawn(async move {
                    if let Ok(store) = DeskStore::open().await {
                        let _ = DeskPersistence::new(store, &demo_world()).forget().await;
                    }
                    generation += 1;
                });
            },
        }
    }
}

#[component]
fn DemoDesk(on_reset: EventHandler<MouseEvent>) -> Element {
    let booted = use_resource(boot_demo);
    let booted: Option<Result<Session, String>> = booted.read().clone();
    rsx! {
        document::Link { rel: "stylesheet", href: DESK_CSS }
        match booted {
            Some(Ok(session)) => rsx! {
                Desk {
                    session,
                    mode: "demo world",
                    toolbar: rsx! {
                        button { class: "quiet", onclick: move |event| on_reset.call(event), "reset demo" }
                    },
                }
            },
            Some(Err(error)) => rsx! { div { class: "failed", "the demo world failed to boot: {error}" } },
            None => rsx! { div { class: "loading", "building the demo world…" } },
        }
    }
}

fn demo_world() -> String {
    format!("demo:{SYNTH_SPEC}")
}

async fn boot_demo() -> Result<Session, String> {
    let mut source = FixtureSource::from_synth_spec(SYNTH_SPEC).map_err(|e| e.to_string())?;
    let (config, _skipped) = source.derived_config();
    install_rehearsal(&mut source, &config, SPEED, now_millis())
        .await
        .map_err(|e| e.to_string())?;
    let env = PreflightEnv {
        notify: &|_| {},
        roster_dir: None,
        roster_write: false,
        rate_limit_waits: 0,
    };
    let report = preflight(&source, &config, PREFLIGHT_TIMEOUT, true, classify_fixture_error, &env).await;
    let now = now_millis();
    let mut state = AppState::new(config, report.writes_armed, report.into_bootstraps(), now);
    // The fixture always answers, so only the overlay is restored (no snapshot seeding).
    let (persistence, saved) = match DeskStore::open().await {
        Ok(store) => {
            let persistence = Rc::new(DeskPersistence::new(store, &demo_world()));
            let saved = peek_saved(&persistence).await;
            (Some(persistence), saved)
        }
        Err(error) => (None, SavedState::Unavailable(error)),
    };
    apply_saved(&mut state, saved, now);
    Ok(start(Rc::new(source), state, classify_fixture_error, persistence))
}

//! A token-free world for trying the desk: a synthetic tournament played
//! forward on a fast clock, served by the fixture source.

use std::{rc::Rc, time::Duration};

use bracket_tools_scheduler_core::{
    fixture_source::{classify_fixture_error, FixtureSource},
    preflight::{preflight, PreflightEnv},
    rehearsal::install_rehearsal,
    timers::now_millis,
};
use dioxus::prelude::*;

use crate::{
    bridge::{start, Session},
    views::Desk,
    DESK_CSS,
};

/// A 32-player double elimination plus an 8-player round robin sharing players.
const SYNTH_SPEC: &str = "de:32,rr:8";
/// Wall-clock speed-up for the scripted completions.
const SPEED: f64 = 20.0;
const PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(10);

/// The scheduler over the demo world, booted on mount.
#[component]
pub fn DemoTool() -> Element {
    let booted = use_resource(boot_demo);
    let booted: Option<Result<Session, String>> = booted.read().clone();
    rsx! {
        document::Link { rel: "stylesheet", href: DESK_CSS }
        match booted {
            Some(Ok(session)) => rsx! { Desk { session, mode: "demo world" } },
            Some(Err(error)) => rsx! { div { class: "failed", "the demo world failed to boot: {error}" } },
            None => rsx! { div { class: "loading", "building the demo world…" } },
        }
    }
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
    let writes_armed = report.writes_armed;
    Ok(start(
        Rc::new(source),
        config,
        writes_armed,
        report.into_bootstraps(),
        classify_fixture_error,
    ))
}

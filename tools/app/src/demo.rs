//! A token-free world for trying the desk: a synthetic tournament played
//! forward on a fast clock, served by the fixture source.

use std::{rc::Rc, time::Duration};

use bracket_tools_scheduler_core::{
    fixture_source::{classify_fixture_error, FixtureSource},
    preflight::{preflight, PreflightEnv},
    rehearsal::install_rehearsal,
    timers::now_millis,
};

use crate::bridge::{start, Session};

/// A 32-player double elimination plus an 8-player round robin sharing players.
const SYNTH_SPEC: &str = "de:32,rr:8";
/// Wall-clock speed-up for the scripted completions.
const SPEED: f64 = 20.0;
const PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(10);

pub async fn boot_demo() -> Result<Session, String> {
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
    Ok(start(Rc::new(source), config, report.into_bootstraps()))
}

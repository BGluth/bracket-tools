//! The polling task: full snapshot cycles over every configured event, plus
//! immediate targeted force-polls (freed setups awaiting a result).
//!
//! Cycles are strictly sequential — one task runs them, so a slow cycle
//! delays rather than overlaps the next. Within a cycle, events fetch with
//! bounded concurrency and a per-request timeout, so one wedged event costs
//! its timeout, not the cycle.
//!
//! The tearing guard (retain a set that vanished from one successful fetch
//! for a grace cycle) lives app-side in `apply_snapshot`, where the previous
//! snapshot already exists to diff against.

use std::{
    collections::HashSet,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use bracket_tools_startgg::GGProviderError;
use cynic::http::CynicReqwestError;
use futures::{stream, StreamExt};
use tokio::{
    sync::mpsc::{UnboundedReceiver, UnboundedSender},
    time::{sleep_until, timeout, Instant},
};

use crate::{
    app::{Msg, PollFailure, PollOutcome, PollResult},
    conflict::UnixMillis,
    model::{live_sets_from_schema, BracketId},
    set_source::SetSource,
};

pub const POLL_CONCURRENCY: usize = 3;

#[derive(Debug, Clone)]
pub struct PollerConfig {
    pub interval: Duration,
    pub request_timeout: Duration,
    pub concurrency: usize,
}

impl PollerConfig {
    pub fn from_scheduler(config: &crate::config::SchedulerConfig) -> Self {
        Self {
            interval: Duration::from_secs(config.poll_interval_secs),
            request_timeout: Duration::from_secs(20),
            concurrency: POLL_CONCURRENCY,
        }
    }
}

/// One full snapshot pass over `events`. Every event yields a [`PollResult`]
/// — snapshot or classified failure — stamped with this cycle's `seq`.
pub async fn poll_cycle<S: SetSource>(
    source: &S,
    events: &[BracketId],
    seq: u64,
    config: &PollerConfig,
    classify: &impl Fn(&S::Error) -> PollFailure,
) -> Vec<PollResult> {
    stream::iter(events.iter().cloned())
        .map(|bracket| async move {
            let outcome = match timeout(config.request_timeout, source.fetch_event_sets(&bracket.0)).await {
                Err(_elapsed) => PollOutcome::Failed(PollFailure::Offline),
                Ok(Err(error)) => PollOutcome::Failed(classify(&error)),
                Ok(Ok(schema_sets)) => {
                    let (sets, warnings, skipped) = live_sets_from_schema(schema_sets);
                    PollOutcome::Snapshot { sets, warnings, skipped }
                }
            };
            PollResult {
                bracket,
                seq,
                captured_at: now_millis(),
                outcome,
            }
        })
        .buffer_unordered(config.concurrency)
        .collect()
        .await
}

/// The long-running poll loop: a full cycle per interval, with queued
/// force-poll targets serviced immediately between cycles. Exits when the
/// app side of either channel closes.
pub async fn run_poller<S, F>(
    source: &S,
    events: Vec<BracketId>,
    config: PollerConfig,
    classify: F,
    tx: UnboundedSender<Msg>,
    mut force_rx: UnboundedReceiver<BracketId>,
) where
    S: SetSource,
    F: Fn(&S::Error) -> PollFailure,
{
    let mut seq = 0u64;
    loop {
        seq += 1;
        for result in poll_cycle(source, &events, seq, &config, &classify).await {
            if tx.send(Msg::Poll(result)).is_err() {
                return;
            }
        }

        let deadline = Instant::now() + config.interval;
        loop {
            tokio::select! {
                _ = sleep_until(deadline) => break,
                forced = force_rx.recv() => {
                    let Some(first) = forced else { return };
                    // Coalesce every queued request into one targeted pass.
                    let mut targets = HashSet::from([first]);
                    while let Ok(more) = force_rx.try_recv() {
                        targets.insert(more);
                    }
                    let targets: Vec<BracketId> = targets.into_iter().collect();
                    seq += 1;
                    for result in poll_cycle(source, &targets, seq, &config, &classify).await {
                        if tx.send(Msg::Poll(result)).is_err() {
                            return;
                        }
                    }
                }
            }
        }
    }
}

/// Three-bucket classification for the live provider's errors.
pub fn classify_provider_error(error: &GGProviderError) -> PollFailure {
    match error {
        GGProviderError::Http(CynicReqwestError::ReqwestError(e)) if e.is_connect() || e.is_timeout() => PollFailure::Offline,
        GGProviderError::Http(CynicReqwestError::ReqwestError(_)) => PollFailure::Transient,
        GGProviderError::Http(CynicReqwestError::ErrorResponse(status, body)) => {
            if *status == 429 || status.is_server_error() {
                PollFailure::Transient
            } else {
                PollFailure::Persistent(format!("{status}: {body:.120}"))
            }
        }
        // Bad slug / permissions / schema drift: retrying won't fix these.
        GGProviderError::GraphQl(_) | GGProviderError::Conversion(_) | GGProviderError::EmptyResponse => {
            PollFailure::Persistent(error.to_string())
        }
        GGProviderError::Storage(_) | GGProviderError::CacheDeserialization(_) => PollFailure::Transient,
    }
}

fn now_millis() -> UnixMillis {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use std::{slice::from_ref, str::FromStr, sync::Arc, time::Duration};

    use bracket_tools_startgg::{types::GGRestToken, GGProvider};
    use tokio::sync::mpsc::unbounded_channel;

    use super::{classify_provider_error, poll_cycle, run_poller, PollerConfig};
    use crate::{
        app::{Msg, PollFailure, PollOutcome},
        fixture_source::{FixtureError, FixtureSource},
        model::BracketId,
        set_source::StartggSource,
        synth::make_de_bracket,
    };

    const SLUG: &str = "tournament/synth/event/ultimate";
    const HANG_SLUG: &str = "tournament/synth/event/wedged";

    fn config(timeout_ms: u64) -> PollerConfig {
        PollerConfig {
            interval: Duration::from_secs(300),
            request_timeout: Duration::from_millis(timeout_ms),
            concurrency: 3,
        }
    }

    fn classify(_: &FixtureError) -> PollFailure {
        PollFailure::Persistent("unknown event".to_owned())
    }

    fn two_event_source() -> FixtureSource {
        let bracket = make_de_bracket(1001, 8);
        let wedged = make_de_bracket(2001, 8);
        let mut source = FixtureSource::new();
        source.add_synth_event(SLUG, from_ref(&bracket.info), vec![bracket.sets]);
        source.add_synth_event(HANG_SLUG, from_ref(&wedged.info), vec![wedged.sets]);
        source
    }

    #[tokio::test]
    async fn wedged_event_times_out_without_stalling_the_cycle() {
        let mut source = two_event_source();
        source.set_hang(HANG_SLUG);
        let events = vec![BracketId(SLUG.to_owned()), BracketId(HANG_SLUG.to_owned())];

        let results = poll_cycle(&source, &events, 1, &config(50), &classify).await;

        assert_eq!(results.len(), 2);
        let healthy = results.iter().find(|r| r.bracket.0 == SLUG).unwrap();
        assert!(matches!(healthy.outcome, PollOutcome::Snapshot { .. }));
        let wedged = results.iter().find(|r| r.bracket.0 == HANG_SLUG).unwrap();
        assert!(
            matches!(wedged.outcome, PollOutcome::Failed(PollFailure::Offline)),
            "a hung fetch classifies as a failed (offline) cycle"
        );
        assert!(results.iter().all(|r| r.seq == 1));
    }

    #[tokio::test]
    async fn source_errors_run_through_the_classifier() {
        let source = two_event_source();
        let events = vec![BracketId("tournament/synth/event/nonexistent".to_owned())];

        let results = poll_cycle(&source, &events, 7, &config(1000), &classify).await;
        assert!(matches!(
            &results[0].outcome,
            PollOutcome::Failed(PollFailure::Persistent(msg)) if msg == "unknown event"
        ));
        assert_eq!(results[0].seq, 7);
    }

    /// The S1 blocking item, upgraded to the real loop: `tokio::spawn`
    /// requires `Send`, so this compiling proves the whole
    /// run_poller → SetSource → provider chain stays `Send` with plain
    /// `async fn` in the trait (RPITIT).
    #[tokio::test]
    async fn live_poller_future_is_send() {
        let token = GGRestToken::from_str("fake-token-for-compile-spike").unwrap();
        let provider = GGProvider::builder(token).build().unwrap();
        let source = Arc::new(StartggSource::new(provider));
        let (tx, _rx) = unbounded_channel();
        let (_force_tx, force_rx) = unbounded_channel();

        let handle = tokio::spawn(async move {
            run_poller(&*source, Vec::new(), config(1000), classify_provider_error, tx, force_rx).await;
        });
        handle.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn force_poll_targets_one_event_between_cycles() {
        let source = Arc::new(two_event_source());
        let events = vec![BracketId(SLUG.to_owned()), BracketId(HANG_SLUG.to_owned())];
        let (tx, mut rx) = unbounded_channel();
        let (force_tx, force_rx) = unbounded_channel();

        let poller_source = source.clone();
        let handle = tokio::spawn(async move {
            run_poller(&*poller_source, events, config(1000), classify, tx, force_rx).await;
        });

        // Cycle 1 covers both events.
        let mut cycle1 = Vec::new();
        for _ in 0..2 {
            let Some(Msg::Poll(result)) = rx.recv().await else {
                panic!("expected a poll result");
            };
            cycle1.push(result);
        }
        assert!(cycle1.iter().all(|r| r.seq == 1));

        // A force request is serviced immediately (virtual time: no 300s
        // interval has elapsed) and targets only the requested event.
        force_tx.send(BracketId(SLUG.to_owned())).unwrap();
        let Some(Msg::Poll(forced)) = rx.recv().await else {
            panic!("expected the forced poll result");
        };
        assert_eq!(forced.bracket.0, SLUG);
        assert_eq!(forced.seq, 2);

        handle.abort();
    }
}

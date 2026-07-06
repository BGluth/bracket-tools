//! The write task: performs the update loop's mutation intents against the
//! source, one at a time, and reports every outcome back as a
//! [`Msg::Write`].
//!
//! Sequential processing gives single-flight per set for free (no two
//! mutations are ever in flight at once, let alone for one set). Failures
//! classify into the same three buckets as reads: connectivity/transient
//! failures retry with linear backoff up to a cap, definitive failures park
//! immediately (the app keeps them visible for the TO).
//!
//! Successful mutations also bracket the server-clock offset: `startedAt` in
//! the payload is stamped by the mutation itself, so server time minus the
//! local send/receive midpoint is a one-RTT-tight offset sample.
//!
//! Flush discipline: a connectivity-class failure (offline, request timeout)
//! doesn't burn retry attempts — the intent goes back to the app as
//! AwaitReconnect and is re-released only once its target event polls
//! successfully again (revalidated there against the fresh snapshot). 429/5xx
//! server trouble keeps the bounded in-writer backoff, then parks visibly.
//!
//! TODO(S4): limiter headroom priority for mutations over poll pages — the
//! shared governor lives inside GGProvider today, so both paths contend
//! equally; acceptable for FBR volumes.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bracket_tools_startgg::SetMutationResult;
use tokio::{
    sync::mpsc::{UnboundedReceiver, UnboundedSender},
    time::{sleep, timeout},
};

use crate::{
    app::{Msg, OffsetSample, PollFailure, WriteIntent, WriteKind, WriteOutcome, WriteResult},
    conflict::UnixMillis,
    set_source::SetSource,
};

#[derive(Debug, Clone)]
pub struct WriterConfig {
    pub request_timeout: Duration,
    /// Total tries for retryable failures before parking.
    pub max_attempts: u32,
    /// Linear backoff unit: attempt N sleeps N × this before retrying.
    pub retry_backoff: Duration,
}

impl Default for WriterConfig {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(20),
            max_attempts: 5,
            retry_backoff: Duration::from_secs(2),
        }
    }
}

enum AttemptFailure {
    /// Offline / timed out: hand back to the app until the event polls again.
    Connectivity(String),
    /// Server trouble (429/5xx): bounded in-writer retries.
    Retryable(String),
    Definitive(String),
}

/// Consumes intents until the channel closes. Every intent ends in exactly
/// one Success/Terminal/AwaitReconnect result; each failed retryable attempt
/// additionally reports a Transient result (the pending view shows liveness,
/// not silence).
pub async fn run_writer<S, F>(
    source: &S,
    config: WriterConfig,
    classify: F,
    tx: UnboundedSender<Msg>,
    mut rx: UnboundedReceiver<WriteIntent>,
) where
    S: SetSource,
    F: Fn(&S::Error) -> PollFailure,
{
    while let Some(intent) = rx.recv().await {
        let mut attempts = 0u32;
        let outcome = loop {
            attempts += 1;
            match write_attempt(source, &intent, config.request_timeout, &classify).await {
                Ok((payload, offset)) => break WriteOutcome::Success { payload, offset },
                Err(AttemptFailure::Definitive(error)) => break WriteOutcome::Terminal { error },
                Err(AttemptFailure::Connectivity(error)) => break WriteOutcome::AwaitReconnect { error },
                Err(AttemptFailure::Retryable(error)) => {
                    if attempts >= config.max_attempts {
                        break WriteOutcome::Terminal {
                            error: format!("gave up after {attempts} attempts: {error}"),
                        };
                    }
                    let report = Msg::Write(WriteResult {
                        intent: intent.clone(),
                        outcome: WriteOutcome::Transient { error, attempts },
                    });
                    if tx.send(report).is_err() {
                        return;
                    }
                    sleep(config.retry_backoff * attempts).await;
                }
            }
        };
        let done = Msg::Write(WriteResult { intent, outcome });
        if tx.send(done).is_err() {
            return;
        }
    }
}

async fn write_attempt<S, F>(
    source: &S,
    intent: &WriteIntent,
    request_timeout: Duration,
    classify: &F,
) -> Result<(SetMutationResult, Option<OffsetSample>), AttemptFailure>
where
    S: SetSource,
    F: Fn(&S::Error) -> PollFailure,
{
    let mutation = async {
        match intent.kind {
            WriteKind::Called => source.mark_called(intent.id).await,
            WriteKind::InProgress => source.mark_in_progress(intent.id).await,
        }
    };
    let sent_at = now_millis();
    match timeout(request_timeout, mutation).await {
        Err(_elapsed) => Err(AttemptFailure::Connectivity("request timed out".to_owned())),
        Ok(Err(error)) => match classify(&error) {
            PollFailure::Offline => Err(AttemptFailure::Connectivity(error.to_string())),
            PollFailure::Transient => Err(AttemptFailure::Retryable(error.to_string())),
            PollFailure::Persistent(msg) => Err(AttemptFailure::Definitive(msg)),
        },
        Ok(Ok(payload)) => {
            let offset = offset_sample(&payload, sent_at, now_millis());
            Ok((payload, offset))
        }
    }
}

/// `startedAt` (server seconds) minus the local send/receive midpoint. None
/// when the payload carried no server timestamp.
fn offset_sample(payload: &SetMutationResult, sent_at: UnixMillis, received_at: UnixMillis) -> Option<OffsetSample> {
    let started = payload.started_at.as_ref()?;
    Some(OffsetSample {
        offset_secs: started.0 - (sent_at + received_at) / 2 / 1000,
        at: received_at,
    })
}

fn now_millis() -> UnixMillis {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use std::{
        future::pending,
        sync::atomic::{AtomicU32, Ordering},
        time::Duration,
    };

    use bracket_tools_startgg::{SetMutationResult, StartGgId};
    use bracket_tools_startgg_schema::{get_event_structure, get_sets_for_event, scalars::Timestamp};
    use tokio::sync::mpsc::unbounded_channel;

    use super::{run_writer, WriterConfig};
    use crate::{
        app::{Msg, PollFailure, WriteIntent, WriteKind, WriteOutcome},
        fixture_source::{FixtureError, FixtureSource},
        model::{BracketId, SetKey},
        set_source::SetSource,
    };

    const NOW: i64 = 1_751_000_000_000;

    fn test_config() -> WriterConfig {
        WriterConfig {
            request_timeout: Duration::from_millis(50),
            max_attempts: 3,
            retry_backoff: Duration::from_millis(10),
        }
    }

    fn intent(kind: WriteKind) -> WriteIntent {
        WriteIntent {
            bracket: BracketId("ultimate".to_owned()),
            key: SetKey {
                phase_group: "1001".to_owned(),
                round: 1,
                identifier: "A".to_owned(),
            },
            id: 4242,
            kind,
            created_at: NOW,
        }
    }

    fn classify(_: &FixtureError) -> PollFailure {
        PollFailure::Persistent("definitive".to_owned())
    }

    /// A source whose mutations misbehave a scripted number of times.
    struct FlakySource {
        failures_before_success: u32,
        calls: AtomicU32,
        hang: bool,
        server_started_at: Option<i64>,
    }

    impl SetSource for FlakySource {
        type Error = FixtureError;

        async fn fetch_event_sets(&self, _: &str) -> Result<Vec<get_sets_for_event::Set>, Self::Error> {
            unreachable!("writer never reads")
        }

        async fn fetch_event_structure(&self, _: &str) -> Result<get_event_structure::Event, Self::Error> {
            unreachable!("writer never reads")
        }

        async fn mark_called(&self, set_id: StartGgId) -> Result<SetMutationResult, Self::Error> {
            if self.hang {
                pending::<()>().await;
            }
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call < self.failures_before_success {
                Err(FixtureError::UnknownEvent("flaky".to_owned()))
            } else {
                Ok(SetMutationResult {
                    id: Some(set_id),
                    state: Some(6),
                    started_at: self.server_started_at.map(Timestamp),
                    completed_at: None,
                })
            }
        }

        async fn mark_in_progress(&self, set_id: StartGgId) -> Result<SetMutationResult, Self::Error> {
            self.mark_called(set_id).await
        }
    }

    async fn drive(source: FlakySource, classify: fn(&FixtureError) -> PollFailure, sent: WriteIntent) -> Vec<WriteOutcome> {
        let (tx, mut rx) = unbounded_channel();
        let (intent_tx, intent_rx) = unbounded_channel();
        intent_tx.send(sent).unwrap();
        drop(intent_tx); // channel closes once the intent is consumed

        run_writer(&source, test_config(), classify, tx, intent_rx).await;

        let mut outcomes = Vec::new();
        while let Ok(Msg::Write(result)) = rx.try_recv() {
            outcomes.push(result.outcome);
        }
        outcomes
    }

    #[tokio::test]
    async fn success_reports_the_mutation_payload() {
        let source = FixtureSource::new();
        let (tx, mut rx) = unbounded_channel();
        let (intent_tx, intent_rx) = unbounded_channel();
        intent_tx.send(intent(WriteKind::Called)).unwrap();
        intent_tx.send(intent(WriteKind::InProgress)).unwrap();
        drop(intent_tx);

        run_writer(&source, test_config(), classify, tx, intent_rx).await;

        let Some(Msg::Write(first)) = rx.recv().await else { panic!() };
        let WriteOutcome::Success { payload, .. } = first.outcome else {
            panic!("expected success: {:?}", first.outcome);
        };
        assert_eq!(payload.state, Some(6));
        let Some(Msg::Write(second)) = rx.recv().await else { panic!() };
        assert!(matches!(second.outcome, WriteOutcome::Success { .. }));
        assert_eq!(source.mutation_log().len(), 2, "both mutations reached the source in order");
    }

    #[tokio::test]
    async fn success_with_started_at_brackets_a_clock_offset() {
        // Server clock reads 100s ahead of ours.
        let ahead = 100i64;
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let source = FlakySource {
            failures_before_success: 0,
            calls: AtomicU32::new(0),
            hang: false,
            server_started_at: Some(now_secs + ahead),
        };
        let outcomes = drive(source, classify, intent(WriteKind::Called)).await;

        let Some(WriteOutcome::Success { offset: Some(sample), .. }) = outcomes.last() else {
            panic!("expected a success with an offset sample: {outcomes:?}");
        };
        assert!(
            (sample.offset_secs - ahead).abs() <= 5,
            "offset ≈ +{ahead}s, got {}",
            sample.offset_secs
        );
    }

    #[tokio::test]
    async fn definitive_failure_parks_immediately() {
        let source = FlakySource {
            failures_before_success: u32::MAX,
            calls: AtomicU32::new(0),
            hang: false,
            server_started_at: None,
        };
        let outcomes = drive(source, classify, intent(WriteKind::Called)).await;

        assert_eq!(outcomes.len(), 1, "no retries on a definitive failure: {outcomes:?}");
        assert!(matches!(&outcomes[0], WriteOutcome::Terminal { error } if error == "definitive"));
    }

    #[tokio::test(start_paused = true)]
    async fn transient_failures_retry_then_succeed() {
        fn transient(_: &FixtureError) -> PollFailure {
            PollFailure::Transient
        }
        let source = FlakySource {
            failures_before_success: 2,
            calls: AtomicU32::new(0),
            hang: false,
            server_started_at: None,
        };
        let outcomes = drive(source, transient, intent(WriteKind::Called)).await;

        assert_eq!(outcomes.len(), 3, "{outcomes:?}");
        assert!(matches!(outcomes[0], WriteOutcome::Transient { attempts: 1, .. }));
        assert!(matches!(outcomes[1], WriteOutcome::Transient { attempts: 2, .. }));
        assert!(matches!(outcomes[2], WriteOutcome::Success { .. }));
    }

    #[tokio::test(start_paused = true)]
    async fn hung_mutations_hand_back_for_reconnect() {
        let source = FlakySource {
            failures_before_success: 0,
            calls: AtomicU32::new(0),
            hang: true,
            server_started_at: None,
        };
        let outcomes = drive(source, classify, intent(WriteKind::Called)).await;

        // A hang is connectivity-class: one timeout, no burned attempts, the
        // intent goes back to the app to await its event's next poll.
        assert_eq!(outcomes.len(), 1, "{outcomes:?}");
        let WriteOutcome::AwaitReconnect { error } = &outcomes[0] else {
            panic!("expected reconnect hand-back: {outcomes:?}");
        };
        assert!(error.contains("timed out"), "{error}");
    }

    #[tokio::test]
    async fn offline_classified_errors_hand_back_for_reconnect() {
        fn offline(_: &FixtureError) -> PollFailure {
            PollFailure::Offline
        }
        let source = FlakySource {
            failures_before_success: u32::MAX,
            calls: AtomicU32::new(0),
            hang: false,
            server_started_at: None,
        };
        let outcomes = drive(source, offline, intent(WriteKind::Called)).await;

        assert_eq!(outcomes.len(), 1, "{outcomes:?}");
        assert!(matches!(&outcomes[0], WriteOutcome::AwaitReconnect { .. }));
    }
}

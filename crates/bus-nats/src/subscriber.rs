use crate::{
    ack,
    client::NatsClient,
    consumer::build_pull_config,
    dlq::{
        CLASS_PERMANENT, CLASS_POISON, CLASS_TRANSIENT_EXHAUSTED, DlqOptions, FALLBACK_NAK_DELAY,
        FailureInfo, REASON_HANDLER_PERMANENT, REASON_INVALID_PAYLOAD,
        REASON_MAX_RETRIES_EXCEEDED, build_dlq_headers, dlq_subject, publish_to_dlq,
    },
};
use async_nats::jetstream::{
    self, Message,
    consumer::{Consumer, pull},
};
use bus_core::{
    error::{BusError, HandlerError},
    event::Event,
    handler::{EventHandler, HandlerCtx},
    id::MessageId,
    idempotency::{ClaimOutcome, IdempotencyStore},
};
use futures_util::StreamExt;
use std::{str::FromStr, sync::Arc, time::Duration};
use tokio::{sync::Semaphore, task::JoinHandle};
use tracing::Span;

const SECONDS_PER_DAY: u64 = 24 * 60 * 60;

pub const DEFAULT_IDEMPOTENCY_TTL: Duration = Duration::from_secs(7 * SECONDS_PER_DAY);

/// Options for subscribing to events from a JetStream stream.
pub struct SubscribeOptions {
    pub stream: String,
    pub durable: String,
    pub filter: String,
    pub max_deliver: i64,
    pub ack_wait: Duration,
    pub backoff: Vec<Duration>,
    pub concurrency: usize,
    pub dlq: Option<DlqOptions>,
    /// TTL for idempotency keys claimed by `try_claim`. Default: [`DEFAULT_IDEMPOTENCY_TTL`].
    pub idempotency_ttl: Duration,
}

impl Default for SubscribeOptions {
    fn default() -> Self {
        Self {
            stream: "EVENTS".into(),
            durable: "default-worker".into(),
            filter: ">".into(),
            max_deliver: 5,
            ack_wait: Duration::from_secs(30),
            backoff: vec![
                Duration::from_secs(1),
                Duration::from_secs(5),
                Duration::from_secs(30),
                Duration::from_secs(300),
            ],
            concurrency: 1,
            dlq: None,
            idempotency_ttl: DEFAULT_IDEMPOTENCY_TTL,
        }
    }
}

/// Handle to a running subscription. Dropping this handle stops the consumer loop
/// and aborts every in-flight per-message worker.
pub struct SubscriptionHandle {
    _handle: JoinHandle<()>,
}

impl Drop for SubscriptionHandle {
    fn drop(&mut self) {
        // Abort the outer message loop. When the outer task is aborted, its
        // owned `JoinSet<()>` is dropped, which aborts every spawned worker.
        self._handle.abort();
    }
}

#[derive(Clone)]
struct ProcessingOptions {
    dlq_opts: Option<DlqOptions>,
    js: jetstream::Context,
    source: String,
    durable: String,
    max_deliver: i64,
    backoff: Vec<Duration>,
    idempotency_ttl: Duration,
}

struct TerminalFailure<'a> {
    msg_id: &'a MessageId,
    stream_sequence: u64,
    delivered: u64,
    failure_reason: &'static str,
    failure_class: &'static str,
    failure_detail: String,
}

/// Start a pull consumer loop that dispatches messages to `handler`.
/// Idempotency is checked via `idempotency_store` before handler invocation.
pub async fn subscribe<E, H, I>(
    client: NatsClient,
    opts: SubscribeOptions,
    handler: Arc<H>,
    idempotency_store: Arc<I>,
) -> Result<SubscriptionHandle, BusError>
where
    E: Event,
    H: EventHandler<E>,
    I: IdempotencyStore + ?Sized + 'static,
{
    let stream = client
        .js
        .get_stream(&opts.stream)
        .await
        .map_err(|e| BusError::Nats(e.to_string()))?;

    let consumer: Consumer<pull::Config> = stream
        .get_or_create_consumer(
            &opts.durable,
            build_pull_config(
                &opts.durable,
                &opts.filter,
                opts.max_deliver,
                opts.ack_wait,
                &opts.backoff,
            ),
        )
        .await
        .map_err(|e| BusError::Nats(e.to_string()))?;

    let semaphore = Arc::new(Semaphore::new(opts.concurrency));
    let processing_options = ProcessingOptions {
        dlq_opts: opts.dlq.clone(),
        js: client.js.clone(),
        source: opts.stream.clone(),
        durable: opts.durable.clone(),
        max_deliver: opts.max_deliver,
        backoff: opts.backoff.clone(),
        idempotency_ttl: opts.idempotency_ttl,
    };

    let handle = tokio::spawn(async move {
        let mut messages = match consumer.messages().await {
            Ok(stream) => stream,
            Err(error) => {
                tracing::error!("failed to get message stream: {}", error);
                return;
            }
        };

        let mut workers: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();

        loop {
            tokio::select! {
                biased;
                Some(joined) = workers.join_next(), if !workers.is_empty() => {
                    if let Err(error) = joined
                        && !error.is_cancelled()
                    {
                        tracing::warn!("worker task error: {}", error);
                    }
                }
                next = messages.next() => {
                    let Some(item) = next else { break };
                    let msg = match item {
                        Ok(message) => message,
                        Err(error) => {
                            tracing::warn!("message stream error: {}", error);
                            continue;
                        }
                    };

                    let permit = semaphore
                        .clone()
                        .acquire_owned()
                        .await
                        .expect("subscriber semaphore is never closed");
                    let handler = handler.clone();
                    let store = idempotency_store.clone();
                    let processing_options = processing_options.clone();

                    workers.spawn(async move {
                        let _permit = permit;
                        process_message::<E, H, I>(msg, handler, store, processing_options).await;
                    });
                }
            }
        }

        workers.shutdown().await;
    });

    Ok(SubscriptionHandle { _handle: handle })
}

async fn process_message<E, H, I>(
    msg: Message,
    handler: Arc<H>,
    store: Arc<I>,
    processing_options: ProcessingOptions,
) where
    E: Event,
    H: EventHandler<E>,
    I: IdempotencyStore + ?Sized,
{
    let info = match msg.info() {
        Ok(i) => i,
        Err(e) => {
            tracing::error!("failed to get message info: {}", e);
            return;
        }
    };

    // Extract message ID from Nats-Msg-Id header, fall back to a fresh UUIDv7.
    // We use `async_nats::header::NATS_MESSAGE_ID` rather than the raw string
    // `"Nats-Msg-Id"` because async-nats represents this as a typed standard
    // header — looking it up by `&str` constructs a `Custom` variant that
    // never matches the stored `Standard` variant.
    let msg_id = msg
        .headers
        .as_ref()
        .and_then(|h| h.get(async_nats::header::NATS_MESSAGE_ID))
        .and_then(|v| MessageId::from_str(v.as_str()).ok())
        .unwrap_or_else(|| MessageId::from_uuid(uuid::Uuid::now_v7()));

    // Idempotency check — skip if already processed
    match store
        .try_claim(&msg_id, processing_options.idempotency_ttl)
        .await
    {
        Ok(ClaimOutcome::Claimed) => {}
        Ok(ClaimOutcome::AlreadyPending) => {
            tracing::debug!(
                %msg_id,
                delivered = info.delivered,
                "claim is pending — retrying handler",
            );
        }
        Ok(ClaimOutcome::AlreadyDone) => {
            tracing::debug!(%msg_id, "already processed — acking duplicate");
            let _ = ack::double_ack(&msg).await;
            return;
        }
        Err(error) => {
            tracing::warn!(%msg_id, "idempotency store error: {} — NAKing", error);
            let _ = ack::nak_with_delay(&msg, Duration::from_secs(1)).await;
            return;
        }
    }

    // Deserialize event
    let event: E = match serde_json::from_slice(&msg.payload) {
        Ok(e) => e,
        Err(e) => {
            tracing::error!(%msg_id, "failed to deserialize event: {} — sending to DLQ", e);
            handle_terminal_failure(
                &msg,
                store.as_ref(),
                &processing_options,
                TerminalFailure {
                    msg_id: &msg_id,
                    stream_sequence: info.stream_sequence,
                    delivered: info.delivered as u64,
                    failure_reason: REASON_INVALID_PAYLOAD,
                    failure_class: CLASS_POISON,
                    failure_detail: e.to_string(),
                },
            )
            .await;
            return;
        }
    };

    let ctx = HandlerCtx {
        msg_id: msg_id.clone(),
        stream_seq: info.stream_sequence,
        delivered: info.delivered as u64,
        subject: msg.subject.to_string(),
        span: Span::current(),
    };

    match handler.handle(ctx, event).await {
        Ok(()) => {
            let _ = store.mark_done(&msg_id).await;
            let _ = ack::double_ack(&msg).await;
        }
        Err(HandlerError::Transient(reason)) => {
            let attempt = info.delivered as i64;
            let is_final_attempt =
                processing_options.max_deliver > 0 && attempt >= processing_options.max_deliver;

            if is_final_attempt {
                tracing::error!(
                    %msg_id,
                    %reason,
                    attempt,
                    "transient error on final attempt — sending to DLQ"
                );
                handle_terminal_failure(
                    &msg,
                    store.as_ref(),
                    &processing_options,
                    TerminalFailure {
                        msg_id: &msg_id,
                        stream_sequence: info.stream_sequence,
                        delivered: info.delivered as u64,
                        failure_reason: REASON_MAX_RETRIES_EXCEEDED,
                        failure_class: CLASS_TRANSIENT_EXHAUSTED,
                        failure_detail: reason,
                    },
                )
                .await;
            } else {
                let delay = compute_backoff(&processing_options.backoff, attempt);
                tracing::warn!(%msg_id, %reason, attempt, ?delay, "transient error — NAKing");
                release_and_nak(store.as_ref(), &msg, &msg_id, delay).await;
            }
        }
        Err(HandlerError::Permanent(reason)) => {
            tracing::error!(%msg_id, %reason, "permanent error — sending to DLQ");
            handle_terminal_failure(
                &msg,
                store.as_ref(),
                &processing_options,
                TerminalFailure {
                    msg_id: &msg_id,
                    stream_sequence: info.stream_sequence,
                    delivered: info.delivered as u64,
                    failure_reason: REASON_HANDLER_PERMANENT,
                    failure_class: CLASS_PERMANENT,
                    failure_detail: reason,
                },
            )
            .await;
        }
    }
}

async fn handle_terminal_failure<I>(
    msg: &Message,
    store: &I,
    processing_options: &ProcessingOptions,
    failure: TerminalFailure<'_>,
) where
    I: IdempotencyStore + ?Sized,
{
    let Some(dlq_opts) = processing_options.dlq_opts.as_ref() else {
        mark_done_and_term(store, msg, failure.msg_id).await;
        return;
    };

    let failure_info = FailureInfo {
        original_subject: msg.subject.to_string(),
        original_stream: processing_options.source.clone(),
        original_seq: failure.stream_sequence,
        original_msg_id: failure.msg_id.to_string(),
        consumer: processing_options.durable.clone(),
        delivered: failure.delivered,
        failure_reason: failure.failure_reason.to_string(),
        failure_class: failure.failure_class.to_string(),
        failure_detail: failure.failure_detail,
    };
    let headers = build_dlq_headers(&failure_info);
    let subject = dlq_subject(&processing_options.source, &processing_options.durable);

    match publish_to_dlq(
        &processing_options.js,
        &subject,
        msg.payload.clone(),
        headers,
        dlq_opts.config.publish_ack_timeout,
    )
    .await
    {
        Ok(()) => mark_done_and_term(store, msg, failure.msg_id).await,
        Err(error) => {
            tracing::error!(
                msg_id = %failure.msg_id,
                "DLQ publish failed: {} — NAKing for retry",
                error,
            );
            release_and_nak(
                store,
                msg,
                failure.msg_id,
                dlq_opts.config.failure_nak_delay,
            )
            .await;
        }
    }
}

async fn mark_done_and_term<I>(store: &I, msg: &Message, msg_id: &MessageId)
where
    I: IdempotencyStore + ?Sized,
{
    if let Err(error) = store.mark_done(msg_id).await {
        tracing::warn!(%msg_id, "failed to mark idempotency key done: {}", error);
    }
    let _ = ack::term(msg).await;
}

async fn release_and_nak<I>(store: &I, msg: &Message, msg_id: &MessageId, delay: Duration)
where
    I: IdempotencyStore + ?Sized,
{
    if let Err(error) = store.release(msg_id).await {
        tracing::warn!(%msg_id, "failed to release idempotency key for retry: {}", error);
    }
    let _ = ack::nak_with_delay(msg, delay).await;
}

fn compute_backoff(backoff: &[Duration], attempt: i64) -> Duration {
    if backoff.is_empty() {
        return FALLBACK_NAK_DELAY;
    }

    let index = (attempt as usize).saturating_sub(1).min(backoff.len() - 1);
    backoff[index]
}

#[cfg(test)]
mod tests {
    use super::compute_backoff;
    use crate::dlq::FALLBACK_NAK_DELAY;
    use std::time::Duration;

    #[test]
    fn compute_backoff_picks_correct_index() {
        let backoff = vec![
            Duration::from_secs(1),
            Duration::from_secs(5),
            Duration::from_secs(30),
        ];

        assert_eq!(compute_backoff(&backoff, 1), Duration::from_secs(1));
        assert_eq!(compute_backoff(&backoff, 2), Duration::from_secs(5));
        assert_eq!(compute_backoff(&backoff, 3), Duration::from_secs(30));
    }

    #[test]
    fn compute_backoff_saturates_at_last() {
        let backoff = vec![Duration::from_secs(1), Duration::from_secs(5)];

        assert_eq!(compute_backoff(&backoff, 99), Duration::from_secs(5));
    }

    #[test]
    fn compute_backoff_empty_returns_fallback() {
        assert_eq!(compute_backoff(&[], 1), FALLBACK_NAK_DELAY);
    }

    #[test]
    fn compute_backoff_attempt_zero_clamps_to_first() {
        let backoff = vec![Duration::from_secs(1)];

        assert_eq!(compute_backoff(&backoff, 0), Duration::from_secs(1));
    }
}

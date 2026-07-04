use crate::{
    ack,
    client::NatsClient,
    consumer::build_pull_config,
    dlq::{
        CLASS_PERMANENT, CLASS_POISON, CLASS_TRANSIENT_EXHAUSTED, DlqOptions, FALLBACK_NAK_DELAY,
        FailureInfo, REASON_HANDLER_PERMANENT, REASON_INVALID_PAYLOAD, REASON_MAX_RETRIES_EXCEEDED,
        build_dlq_headers, dlq_subject, publish_to_dlq,
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

const RECONNECT_BACKOFF_INITIAL: Duration = Duration::from_millis(200);
const RECONNECT_BACKOFF_MAX: Duration = Duration::from_secs(30);

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
        }
    }
}

/// Handle to a running subscription.
///
/// - [`SubscriptionHandle::drain`] performs a graceful shutdown: stop pulling
///   new messages, wait for in-flight handlers to finish (bounded by a
///   timeout), then stop.
/// - Dropping the handle without draining aborts the loop and every in-flight
///   worker immediately; un-acked messages redeliver after `ack_wait`.
pub struct SubscriptionHandle {
    handle: Option<JoinHandle<()>>,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
}

impl SubscriptionHandle {
    /// Gracefully stop the subscription. Returns `true` when every in-flight
    /// handler finished within `timeout`; on timeout the remaining workers
    /// are aborted and `false` is returned.
    pub async fn drain(mut self, timeout: Duration) -> bool {
        let _ = self.shutdown_tx.send(true);
        let Some(mut handle) = self.handle.take() else {
            return true;
        };
        match tokio::time::timeout(timeout, &mut handle).await {
            Ok(_) => true,
            Err(_) => {
                handle.abort();
                false
            }
        }
    }
}

impl Drop for SubscriptionHandle {
    fn drop(&mut self) {
        // Abort the outer message loop. When the outer task is aborted, its
        // owned `JoinSet<()>` is dropped, which aborts every spawned worker.
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
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
}

struct TerminalFailure<'a> {
    msg_id: &'a MessageId,
    stream_sequence: u64,
    delivered: u64,
    failure_reason: &'static str,
    failure_class: &'static str,
    failure_detail: String,
}

/// Deterministic fallback ID for messages published without a `Nats-Msg-Id`
/// header. UUIDv5 over stream/durable/sequence is stable across redeliveries,
/// so the idempotency store still deduplicates such messages.
fn fallback_message_id(stream: &str, durable: &str, stream_sequence: u64) -> MessageId {
    let name = format!("{stream}:{durable}:{stream_sequence}");
    MessageId::from_uuid(uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_OID,
        name.as_bytes(),
    ))
}

async fn double_ack_or_warn(msg: &Message, msg_id: &MessageId) {
    if let Err(error) = ack::double_ack(msg).await {
        tracing::warn!(%msg_id, "double ack failed — message may be redelivered: {}", error);
    }
}

async fn nak_or_warn(msg: &Message, msg_id: &MessageId, delay: Duration) {
    if let Err(error) = ack::nak_with_delay(msg, delay).await {
        tracing::warn!(%msg_id, "NAK failed — redelivery falls back to ack_wait: {}", error);
    }
}

async fn term_or_warn(msg: &Message, msg_id: &MessageId) {
    if let Err(error) = ack::term(msg).await {
        tracing::warn!(%msg_id, "TERM failed — message may be redelivered: {}", error);
    }
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

    if let Some(dlq_opts) = opts.dlq.as_ref() {
        let dlq_stream_name = crate::dlq::dlq_stream_name(&opts.stream, &opts.durable);
        let dlq_subject = crate::dlq::dlq_subject(&opts.stream, &opts.durable);
        crate::dlq::ensure_dlq_stream(&client.js, &dlq_stream_name, &dlq_subject, &dlq_opts.config)
            .await
            .map_err(|e| BusError::Nats(format!("ensure dlq stream {dlq_stream_name}: {e}")))?;
    }

    let semaphore = Arc::new(Semaphore::new(opts.concurrency));
    let processing_options = ProcessingOptions {
        dlq_opts: opts.dlq.clone(),
        js: client.js.clone(),
        source: opts.stream.clone(),
        durable: opts.durable.clone(),
        max_deliver: opts.max_deliver,
        backoff: opts.backoff.clone(),
    };

    let js = client.js.clone();
    let stream_name = opts.stream.clone();
    let durable = opts.durable.clone();
    let filter = opts.filter.clone();
    let max_deliver = opts.max_deliver;
    let ack_wait = opts.ack_wait;
    let consumer_backoff = opts.backoff.clone();

    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);

    let handle = tokio::spawn(async move {
        let mut workers: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();
        let mut current_consumer = Some(consumer);
        let mut reconnect_backoff = RECONNECT_BACKOFF_INITIAL;

        'reconnect: loop {
            // (Re)acquire the consumer. On the first iteration reuse the one
            // created before spawn; afterwards recreate it, because the stream
            // ending usually means the consumer or connection went away.
            let consumer = match current_consumer.take() {
                Some(c) => c,
                None => {
                    let acquired = async {
                        let stream = js
                            .get_stream(&stream_name)
                            .await
                            .map_err(|e| e.to_string())?;
                        stream
                            .get_or_create_consumer(
                                &durable,
                                build_pull_config(
                                    &durable,
                                    &filter,
                                    max_deliver,
                                    ack_wait,
                                    &consumer_backoff,
                                ),
                            )
                            .await
                            .map_err(|e| e.to_string())
                    }
                    .await;
                    match acquired {
                        Ok(c) => c,
                        Err(error) => {
                            tracing::error!(
                                stream = %stream_name,
                                durable = %durable,
                                "failed to reacquire consumer: {} — retrying after {:?}",
                                error,
                                reconnect_backoff,
                            );
                            tokio::select! {
                                _ = shutdown_rx.changed() => break 'reconnect,
                                _ = tokio::time::sleep(reconnect_backoff) => {}
                            }
                            reconnect_backoff = (reconnect_backoff * 2).min(RECONNECT_BACKOFF_MAX);
                            continue 'reconnect;
                        }
                    }
                }
            };

            let mut messages = match consumer.messages().await {
                Ok(stream) => stream,
                Err(error) => {
                    tracing::error!(
                        "failed to get message stream: {} — retrying after {:?}",
                        error,
                        reconnect_backoff,
                    );
                    tokio::select! {
                        _ = shutdown_rx.changed() => break 'reconnect,
                        _ = tokio::time::sleep(reconnect_backoff) => {}
                    }
                    reconnect_backoff = (reconnect_backoff * 2).min(RECONNECT_BACKOFF_MAX);
                    continue 'reconnect;
                }
            };
            reconnect_backoff = RECONNECT_BACKOFF_INITIAL;

            loop {
                tokio::select! {
                    biased;
                    _ = shutdown_rx.changed() => {
                        tracing::info!(
                            stream = %stream_name,
                            durable = %durable,
                            "subscription draining — waiting for in-flight workers",
                        );
                        break 'reconnect;
                    }
                    Some(joined) = workers.join_next(), if !workers.is_empty() => {
                        if let Err(error) = joined
                            && !error.is_cancelled()
                        {
                            tracing::warn!("worker task error: {}", error);
                        }
                    }
                    next = messages.next() => {
                        let Some(item) = next else {
                            tracing::warn!(
                                stream = %stream_name,
                                durable = %durable,
                                "message stream ended — reconnecting after {:?}",
                                reconnect_backoff,
                            );
                            tokio::select! {
                                _ = shutdown_rx.changed() => break 'reconnect,
                                _ = tokio::time::sleep(reconnect_backoff) => {}
                            }
                            reconnect_backoff =
                                (reconnect_backoff * 2).min(RECONNECT_BACKOFF_MAX);
                            continue 'reconnect;
                        };
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
        }

        while workers.join_next().await.is_some() {}
    });

    Ok(SubscriptionHandle {
        handle: Some(handle),
        shutdown_tx,
    })
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
            tracing::error!(
                "failed to get message info: {} — leaving for ack_wait redelivery",
                e
            );
            return;
        }
    };

    // Extract message ID from Nats-Msg-Id header, fall back to a deterministic
    // UUIDv5 derived from stream/durable/sequence.
    // We use `async_nats::header::NATS_MESSAGE_ID` rather than the raw string
    // `"Nats-Msg-Id"` because async-nats represents this as a typed standard
    // header — looking it up by `&str` constructs a `Custom` variant that
    // never matches the stored `Standard` variant.
    let msg_id = msg
        .headers
        .as_ref()
        .and_then(|h| h.get(async_nats::header::NATS_MESSAGE_ID))
        .and_then(|v| MessageId::from_str(v.as_str()).ok())
        .unwrap_or_else(|| {
            let fallback = fallback_message_id(
                &processing_options.source,
                &processing_options.durable,
                info.stream_sequence,
            );
            tracing::warn!(
                msg_id = %fallback,
                subject = %msg.subject,
                "message has no Nats-Msg-Id header — using sequence-derived fallback id",
            );
            fallback
        });

    // Idempotency check — skip if already processed
    match store.try_claim(&msg_id).await {
        Ok(ClaimOutcome::Claimed) => {}
        Ok(ClaimOutcome::AlreadyPending) => {
            // Another worker may be executing the handler for this message
            // right now. Running it here would break effectively-once, so
            // NAK and let a later redelivery observe the final claim state
            // (released -> retry, done -> skip).
            tracing::debug!(
                %msg_id,
                delivered = info.delivered,
                "claim is pending elsewhere — NAKing for later redelivery",
            );
            nak_or_warn(&msg, &msg_id, Duration::from_secs(1)).await;
            return;
        }
        Ok(ClaimOutcome::AlreadyDone) => {
            tracing::debug!(%msg_id, "already processed — acking duplicate");
            double_ack_or_warn(&msg, &msg_id).await;
            return;
        }
        Err(error) => {
            tracing::warn!(%msg_id, "idempotency store error: {} — NAKing", error);
            nak_or_warn(&msg, &msg_id, Duration::from_secs(1)).await;
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
            // mark_done BEFORE ack: if mark_done fails we still ack (the
            // side effects already ran once; re-running the handler would be
            // worse), but the error must be visible to operators because the
            // claim is now stuck pending until its TTL expires.
            if let Err(error) = store.mark_done(&msg_id).await {
                tracing::error!(
                    %msg_id,
                    "mark_done failed after successful handler — claim stuck pending until TTL: {}",
                    error,
                );
            }
            double_ack_or_warn(&msg, &msg_id).await;
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
    term_or_warn(msg, msg_id).await;
}

async fn release_and_nak<I>(store: &I, msg: &Message, msg_id: &MessageId, delay: Duration)
where
    I: IdempotencyStore + ?Sized,
{
    if let Err(error) = store.release(msg_id).await {
        tracing::warn!(%msg_id, "failed to release idempotency key for retry: {}", error);
    }
    nak_or_warn(msg, msg_id, delay).await;
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
    use super::{compute_backoff, fallback_message_id};
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

    #[test]
    fn fallback_message_id_is_deterministic() {
        let a = fallback_message_id("EVENTS", "worker-1", 42);
        let b = fallback_message_id("EVENTS", "worker-1", 42);
        assert_eq!(a, b, "same stream/durable/sequence must yield same id");
    }

    #[test]
    fn fallback_message_id_differs_per_sequence() {
        let a = fallback_message_id("EVENTS", "worker-1", 42);
        let b = fallback_message_id("EVENTS", "worker-1", 43);
        assert_ne!(a, b);
    }
}

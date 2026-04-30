use crate::{ack, client::NatsClient, consumer::build_pull_config};
use async_nats::jetstream::consumer::{pull, Consumer};
use bus_core::{
    error::{BusError, HandlerError},
    event::Event,
    handler::{EventHandler, HandlerCtx},
    id::MessageId,
    idempotency::IdempotencyStore,
};
use futures_util::StreamExt;
use std::{str::FromStr, sync::Arc, time::Duration};
use tokio::{sync::Semaphore, task::JoinHandle};
use tracing::Span;

/// Options for subscribing to events from a JetStream stream.
pub struct SubscribeOptions {
    pub stream:      String,
    pub durable:     String,
    pub filter:      String,
    pub max_deliver: i64,
    pub ack_wait:    Duration,
    pub backoff:     Vec<Duration>,
    pub concurrency: usize,
    pub dlq_subject: Option<String>,
}

impl Default for SubscribeOptions {
    fn default() -> Self {
        Self {
            stream:      "EVENTS".into(),
            durable:     "default-worker".into(),
            filter:      ">".into(),
            max_deliver: 5,
            ack_wait:    Duration::from_secs(30),
            backoff:     vec![
                Duration::from_secs(1),
                Duration::from_secs(5),
                Duration::from_secs(30),
                Duration::from_secs(300),
            ],
            concurrency: 1,
            dlq_subject: None,
        }
    }
}

/// Handle to a running subscription. Dropping this handle stops the consumer loop.
pub struct SubscriptionHandle {
    _handle: JoinHandle<()>,
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
    let dlq_subject = opts.dlq_subject.clone();
    let js = client.js.clone();

    let handle = tokio::spawn(async move {
        let mut messages = match consumer.messages().await {
            Ok(m) => m,
            Err(e) => {
                tracing::error!("failed to get message stream: {}", e);
                return;
            }
        };

        while let Some(item) = messages.next().await {
            let msg = match item {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!("message stream error: {}", e);
                    continue;
                }
            };

            let permit = semaphore.clone().acquire_owned().await.unwrap();
            let handler = handler.clone();
            let store = idempotency_store.clone();
            let dlq = dlq_subject.clone();
            let js = js.clone();

            tokio::spawn(async move {
                let _permit = permit;
                process_message::<E, H, I>(msg, handler, store, dlq, js).await;
            });
        }
    });

    Ok(SubscriptionHandle { _handle: handle })
}

async fn process_message<E, H, I>(
    msg: async_nats::jetstream::Message,
    handler: Arc<H>,
    store: Arc<I>,
    dlq_subject: Option<String>,
    js: async_nats::jetstream::Context,
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

    // Extract message ID from Nats-Msg-Id header, fall back to a fresh UUIDv7
    let msg_id = msg
        .headers
        .as_ref()
        .and_then(|h| h.get("Nats-Msg-Id"))
        .and_then(|v| MessageId::from_str(v.as_str()).ok())
        .unwrap_or_else(|| MessageId::from_uuid(uuid::Uuid::now_v7()));

    // Idempotency check — skip if already processed
    match store.try_insert(&msg_id, Duration::from_secs(86400 * 7)).await {
        Ok(false) => {
            tracing::debug!(%msg_id, "duplicate message — skipping");
            let _ = ack::double_ack(&msg).await;
            return;
        }
        Err(e) => {
            tracing::warn!(%msg_id, "idempotency store error: {} — NAKing", e);
            let _ = ack::nak_with_delay(&msg, Duration::from_secs(1)).await;
            return;
        }
        Ok(true) => {}
    }

    // Deserialize event
    let event: E = match serde_json::from_slice(&msg.payload) {
        Ok(e) => e,
        Err(e) => {
            tracing::error!(%msg_id, "failed to deserialize event: {} — terminating", e);
            let _ = ack::term(&msg).await;
            return;
        }
    };

    let ctx = HandlerCtx {
        msg_id:     msg_id.clone(),
        stream_seq: info.stream_sequence,
        delivered:  info.delivered as u64,
        subject:    msg.subject.to_string(),
        span:       Span::current(),
    };

    match handler.handle(ctx, event).await {
        Ok(()) => {
            let _ = store.mark_done(&msg_id).await;
            let _ = ack::double_ack(&msg).await;
        }
        Err(HandlerError::Transient(reason)) => {
            tracing::warn!(%msg_id, %reason, "transient error — NAKing with backoff");
            let _ = ack::nak_with_delay(&msg, Duration::from_secs(5)).await;
        }
        Err(HandlerError::Permanent(reason)) => {
            tracing::error!(%msg_id, %reason, "permanent error — terminating");
            if let Some(dlq) = dlq_subject {
                let _ = js.publish(dlq, msg.payload.clone()).await;
            }
            let _ = ack::term(&msg).await;
        }
    }
}

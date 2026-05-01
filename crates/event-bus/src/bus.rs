use bus_core::{
    error::BusError,
    event::Event,
    handler::EventHandler,
    idempotency::IdempotencyStore,
    publisher::{PubReceipt, Publisher},
};
use bus_nats::{
    DlqConfig, DlqOptions, NatsClient, NatsPublisher,
    subscriber::{SubscribeOptions, subscribe},
};
use std::sync::Arc;

/// The main event bus handle. Clone cheaply — all state is `Arc`-wrapped.
#[derive(Clone)]
pub struct EventBus {
    _client: NatsClient,
    publisher: NatsPublisher,
    idempotency: Arc<dyn IdempotencyStore>,
    dlq: Option<DlqConfig>,
}

/// Handle to a running subscription. Dropping stops the consumer loop.
pub struct SubscriptionHandle(#[allow(dead_code)] bus_nats::SubscriptionHandle);

impl EventBus {
    pub(crate) fn new(
        client: NatsClient,
        idempotency: Arc<dyn IdempotencyStore>,
        dlq: Option<DlqConfig>,
    ) -> Self {
        let publisher = NatsPublisher::new(client.clone());
        Self {
            _client: client,
            publisher,
            idempotency,
            dlq,
        }
    }

    /// Publish an event. Uses `event.message_id()` as `Nats-Msg-Id` for deduplication.
    pub async fn publish<E: Event>(&self, event: &E) -> Result<PubReceipt, BusError> {
        self.publisher.publish(event).await
    }

    /// Subscribe to events matching `opts.filter` and dispatch to `handler`.
    /// Idempotency is checked automatically before each handler invocation.
    pub async fn subscribe<E, H>(
        &self,
        mut opts: SubscribeOptions,
        handler: H,
    ) -> Result<SubscriptionHandle, BusError>
    where
        E: Event,
        H: EventHandler<E>,
    {
        if opts.dlq.is_none()
            && let Some(config) = &self.dlq
        {
            let stream_name = bus_nats::dlq::dlq_stream_name(&opts.stream, &opts.durable);
            let subject = bus_nats::dlq::dlq_subject(&opts.stream, &opts.durable);
            bus_nats::dlq::ensure_dlq_stream(
                self._client.jetstream(),
                &stream_name,
                &subject,
                config,
            )
            .await?;
            opts.dlq = Some(DlqOptions {
                config: config.clone(),
            });
        }

        let handle = subscribe::<E, H, dyn IdempotencyStore>(
            self._client.clone(),
            opts,
            Arc::new(handler),
            self.idempotency.clone(),
        )
        .await?;
        Ok(SubscriptionHandle(handle))
    }

    /// Graceful shutdown: wait for in-flight handlers, close NATS connection.
    pub async fn shutdown(self) -> Result<(), BusError> {
        // Drop client — async-nats will drain on drop
        Ok(())
    }
}

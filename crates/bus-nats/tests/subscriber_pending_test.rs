use async_trait::async_trait;
use bus_core::{
    EventHandler, HandlerCtx, Publisher,
    error::{BusError, HandlerError},
    id::MessageId,
    idempotency::{ClaimOutcome, IdempotencyStore},
};
use bus_nats::{NatsClient, NatsPublisher, StreamConfig, SubscribeOptions, subscriber::subscribe};
use eventbus_macros::Event;
use serde::{Deserialize, Serialize};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};
use testcontainers::{
    GenericImage, ImageExt,
    core::{IntoContainerPort, WaitFor},
    runners::AsyncRunner,
};

async fn start_nats() -> (impl Drop, String) {
    let c = GenericImage::new("nats", "2.10-alpine")
        .with_exposed_port(4222.tcp())
        .with_wait_for(WaitFor::message_on_stderr("Server is ready"))
        .with_cmd(["-js"])
        .start()
        .await
        .unwrap();
    let host = c.get_host().await.unwrap();
    let port = c.get_host_port_ipv4(4222).await.unwrap();
    (c, format!("nats://{}:{}", host, port))
}

#[derive(Debug, Serialize, Deserialize, Event)]
#[event(subject = "events.pending.created")]
struct PendingEvent {
    id: MessageId,
}

struct CountingHandler(Arc<AtomicU32>);

#[async_trait]
impl EventHandler<PendingEvent> for CountingHandler {
    async fn handle(&self, _ctx: HandlerCtx, _evt: PendingEvent) -> Result<(), HandlerError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// Store that claims to always have a pending claim held elsewhere.
struct AlwaysPendingStore;

#[async_trait]
impl IdempotencyStore for AlwaysPendingStore {
    async fn try_claim(&self, _key: &MessageId) -> Result<ClaimOutcome, BusError> {
        Ok(ClaimOutcome::AlreadyPending)
    }
    async fn mark_done(&self, _key: &MessageId) -> Result<(), BusError> {
        Ok(())
    }
    async fn release(&self, _key: &MessageId) -> Result<(), BusError> {
        Ok(())
    }
}

#[tokio::test]
async fn already_pending_never_runs_handler() {
    let (_c, url) = start_nats().await;
    let cfg = StreamConfig {
        num_replicas: 1,
        ..Default::default()
    };
    let client = NatsClient::connect(&url, &cfg).await.unwrap();
    let publisher = NatsPublisher::new(client.clone());

    let counter = Arc::new(AtomicU32::new(0));
    let handler = Arc::new(CountingHandler(counter.clone()));

    let opts = SubscribeOptions {
        durable: "pending-worker".into(),
        filter: "events.pending.>".into(),
        ..Default::default()
    };

    let _handle =
        subscribe::<PendingEvent, _, _>(client, opts, handler, Arc::new(AlwaysPendingStore))
            .await
            .unwrap();

    publisher
        .publish(&PendingEvent {
            id: MessageId::new(),
        })
        .await
        .unwrap();

    // Give the subscriber time to receive and (wrongly) process the message.
    tokio::time::sleep(Duration::from_secs(2)).await;

    assert_eq!(
        counter.load(Ordering::SeqCst),
        0,
        "handler must not run while a claim is pending elsewhere"
    );
}

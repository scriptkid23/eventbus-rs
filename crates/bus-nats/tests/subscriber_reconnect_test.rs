use async_trait::async_trait;
use bus_core::{EventHandler, HandlerCtx, Publisher, error::HandlerError, id::MessageId};
use bus_nats::{
    NatsClient, NatsKvIdempotencyConfig, NatsKvIdempotencyStore, NatsPublisher, StreamConfig,
    SubscribeOptions, subscriber::subscribe,
};
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
#[event(subject = "events.reconnect.created")]
struct ReconnectEvent {
    id: MessageId,
}

struct CountingHandler(Arc<AtomicU32>);

#[async_trait]
impl EventHandler<ReconnectEvent> for CountingHandler {
    async fn handle(&self, _ctx: HandlerCtx, _evt: ReconnectEvent) -> Result<(), HandlerError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn subscriber_recovers_after_consumer_deleted() {
    let (_c, url) = start_nats().await;
    let cfg = StreamConfig {
        num_replicas: 1,
        ..Default::default()
    };
    let client = NatsClient::connect(&url, &cfg).await.unwrap();
    let publisher = NatsPublisher::new(client.clone());
    let store = Arc::new(
        NatsKvIdempotencyStore::new(
            client.jetstream().clone(),
            NatsKvIdempotencyConfig {
                num_replicas: 1,
                ..Default::default()
            },
        )
        .await
        .unwrap(),
    );

    let counter = Arc::new(AtomicU32::new(0));
    let handler = Arc::new(CountingHandler(counter.clone()));

    let opts = SubscribeOptions {
        durable: "reconnect-worker".into(),
        filter: "events.reconnect.>".into(),
        ..Default::default()
    };

    let _handle = subscribe::<ReconnectEvent, _, _>(client.clone(), opts, handler, store)
        .await
        .unwrap();

    // Sanity: baseline delivery works.
    publisher
        .publish(&ReconnectEvent {
            id: MessageId::new(),
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(counter.load(Ordering::SeqCst), 1);

    // Delete the durable consumer server-side — the message stream ends.
    let stream = client.jetstream().get_stream("EVENTS").await.unwrap();
    stream.delete_consumer("reconnect-worker").await.unwrap();

    // Give the subscriber time to notice and reconnect (backoff starts at 200ms).
    tokio::time::sleep(Duration::from_secs(3)).await;

    publisher
        .publish(&ReconnectEvent {
            id: MessageId::new(),
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(3)).await;

    assert_eq!(
        counter.load(Ordering::SeqCst),
        2,
        "subscriber must recreate the consumer and keep delivering"
    );
}

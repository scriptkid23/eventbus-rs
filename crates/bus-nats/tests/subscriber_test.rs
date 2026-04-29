use async_trait::async_trait;
use bus_core::{EventHandler, HandlerCtx, HandlerError, MessageId, Publisher};
use bus_macros::Event;
use bus_nats::subscriber::subscribe;
use bus_nats::{NatsClient, NatsKvIdempotencyStore, NatsPublisher, StreamConfig, SubscribeOptions};
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
#[event(subject = "events.test.created")]
struct TestEvent {
    id:    MessageId,
    value: u32,
}

struct CountingHandler(Arc<AtomicU32>);

#[async_trait]
impl EventHandler<TestEvent> for CountingHandler {
    async fn handle(&self, _ctx: HandlerCtx, _evt: TestEvent) -> Result<(), HandlerError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn duplicate_event_handled_once() {
    let (_c, url) = start_nats().await;
    let cfg = StreamConfig {
        num_replicas: 1,
        ..Default::default()
    };
    let client = NatsClient::connect(&url, &cfg).await.unwrap();
    let publisher = NatsPublisher::new(client.clone());
    let store = Arc::new(
        NatsKvIdempotencyStore::new(client.jetstream().clone(), Duration::from_secs(60))
            .await
            .unwrap(),
    );

    let counter = Arc::new(AtomicU32::new(0));
    let handler = Arc::new(CountingHandler(counter.clone()));

    let opts = SubscribeOptions {
        durable: "test-worker".into(),
        filter: "events.test.>".into(),
        concurrency: 1,
        ..Default::default()
    };

    let _handle = subscribe::<TestEvent, _, _>(client, opts, handler, store)
        .await
        .unwrap();

    // Publish same event twice (same message_id) — JetStream dedup drops the second
    let evt = TestEvent {
        id:    MessageId::new(),
        value: 42,
    };
    publisher.publish(&evt).await.unwrap();
    publisher.publish(&evt).await.unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "handler must be called exactly once"
    );
}

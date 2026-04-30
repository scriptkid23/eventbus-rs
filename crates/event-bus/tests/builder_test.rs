use bus_core::MessageId;
use bus_macros::Event;
use bus_nats::{NatsKvIdempotencyStore, StreamConfig};
use event_bus::EventBusBuilder;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use testcontainers::{
    core::{IntoContainerPort, WaitFor},
    runners::AsyncRunner,
    ContainerAsync, GenericImage, ImageExt,
};

async fn start_nats() -> (ContainerAsync<GenericImage>, String) {
    let c = GenericImage::new("nats", "2.10-alpine")
        .with_exposed_port(4222.tcp())
        .with_wait_for(WaitFor::message_on_stderr("Server is ready"))
        .with_cmd(["-js"])
        .start()
        .await
        .unwrap();
    let host = c.get_host().await.unwrap();
    let port = c.get_host_port_ipv4(4222).await.unwrap();
    let url = format!("nats://{}:{}", host, port);
    (c, url)
}

#[tokio::test]
async fn builder_requires_idempotency_store() {
    let (_c, url) = start_nats().await;
    // Build without idempotency store — must fail
    let result = EventBusBuilder::new().url(url).build().await;
    assert!(
        result.is_err(),
        "build() without idempotency store must return Err"
    );
}

#[derive(Debug, Serialize, Deserialize, Event)]
#[event(subject = "events.test.created")]
struct TestEvent {
    id:    MessageId,
    value: u32,
}

#[tokio::test]
async fn builder_connects_and_publishes() {
    let (_c, url) = start_nats().await;
    let stream_cfg = StreamConfig {
        num_replicas: 1,
        ..Default::default()
    };
    let client = bus_nats::NatsClient::connect(&url, &stream_cfg)
        .await
        .unwrap();
    let store = NatsKvIdempotencyStore::new(client.jetstream().clone(), Duration::from_secs(3600))
        .await
        .unwrap();

    let bus = EventBusBuilder::new()
        .url(&url)
        .stream_config(stream_cfg)
        .idempotency(store)
        .build()
        .await
        .unwrap();

    let evt = TestEvent {
        id:    MessageId::new(),
        value: 1,
    };
    let receipt = bus.publish(&evt).await.unwrap();
    assert!(!receipt.duplicate);
    assert!(receipt.sequence > 0);

    bus.shutdown().await.unwrap();
}

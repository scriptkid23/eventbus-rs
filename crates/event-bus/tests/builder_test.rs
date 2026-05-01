use async_trait::async_trait;
use bus_core::{EventHandler, HandlerCtx, HandlerError, MessageId};
use bus_macros::Event;
use bus_nats::{DlqConfig, NatsKvIdempotencyStore, StreamConfig, SubscribeOptions};
use event_bus::EventBusBuilder;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use testcontainers::{
    ContainerAsync, GenericImage, ImageExt,
    core::{IntoContainerPort, WaitFor},
    runners::AsyncRunner,
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

async fn connect_client(url: &str, stream_cfg: &StreamConfig) -> bus_nats::NatsClient {
    let mut last_error = None;

    for _ in 0..20 {
        match bus_nats::NatsClient::connect(url, stream_cfg).await {
            Ok(client) => return client,
            Err(error) => {
                last_error = Some(error);
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }

    panic!("failed to connect NATS client: {:?}", last_error);
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
    id: MessageId,
    value: u32,
}

struct AlwaysPermanentHandler;

#[async_trait]
impl EventHandler<TestEvent> for AlwaysPermanentHandler {
    async fn handle(&self, _ctx: HandlerCtx, _evt: TestEvent) -> Result<(), HandlerError> {
        Err(HandlerError::Permanent("always fail".into()))
    }
}

#[tokio::test]
async fn builder_connects_and_publishes() {
    let (_c, url) = start_nats().await;
    let stream_cfg = StreamConfig {
        num_replicas: 1,
        ..Default::default()
    };
    let client = connect_client(&url, &stream_cfg).await;
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
        id: MessageId::new(),
        value: 1,
    };
    let receipt = bus.publish(&evt).await.unwrap();
    assert!(!receipt.duplicate);
    assert!(receipt.sequence > 0);

    bus.shutdown().await.unwrap();
}

#[tokio::test]
async fn builder_with_dlq_auto_wires_per_consumer_dlq_stream() {
    let (_container, url) = start_nats().await;
    let stream_cfg = StreamConfig {
        num_replicas: 1,
        ..Default::default()
    };
    let client = connect_client(&url, &stream_cfg).await;
    let store = NatsKvIdempotencyStore::new(client.jetstream().clone(), Duration::from_secs(3600))
        .await
        .unwrap();

    let bus = EventBusBuilder::new()
        .url(&url)
        .stream_config(stream_cfg)
        .idempotency(store)
        .with_dlq(DlqConfig {
            num_replicas: 1,
            ..Default::default()
        })
        .build()
        .await
        .unwrap();

    let options = SubscribeOptions {
        durable: "auto-dlq-test".into(),
        filter: "events.test.>".into(),
        ..Default::default()
    };

    let _handle = bus
        .subscribe::<TestEvent, _>(options, AlwaysPermanentHandler)
        .await
        .unwrap();

    let dlq_stream = client
        .jetstream()
        .get_stream("DLQ_EVENTS_auto-dlq-test")
        .await;

    assert!(dlq_stream.is_ok());
}

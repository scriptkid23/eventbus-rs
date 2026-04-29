use bus_core::{MessageId, Publisher};
use bus_macros::Event;
use bus_nats::{NatsClient, NatsPublisher, StreamConfig};
use serde::{Deserialize, Serialize};
use testcontainers::{core::{IntoContainerPort, WaitFor}, runners::AsyncRunner, GenericImage, ImageExt};

async fn start_nats() -> (impl Drop, String) {
    let container = GenericImage::new("nats", "2.10-alpine")
        .with_exposed_port(4222.tcp())
        .with_wait_for(WaitFor::message_on_stderr("Server is ready"))
        .with_cmd(["-js"])
        .start()
        .await
        .expect("failed to start NATS");

    let host = container.get_host().await.unwrap();
    let port = container.get_host_port_ipv4(4222).await.unwrap();
    let url = format!("nats://{}:{}", host, port);
    (container, url)
}

#[derive(Debug, Serialize, Deserialize, Event)]
#[event(subject = "events.test.created")]
struct TestEvent {
    id: MessageId,
    value: String,
}

#[tokio::test]
async fn publish_returns_receipt_with_sequence() {
    let (_c, url) = start_nats().await;
    let cfg = StreamConfig { num_replicas: 1, ..StreamConfig::default() };
    let client = NatsClient::connect(&url, &cfg).await.unwrap();
    let publisher = NatsPublisher::new(client);

    let evt = TestEvent { id: MessageId::new(), value: "hello".into() };
    let receipt = publisher.publish(&evt).await.unwrap();

    assert!(!receipt.duplicate);
    assert!(!receipt.buffered);
    assert!(receipt.sequence > 0);
    assert_eq!(receipt.stream, "EVENTS");
}

#[tokio::test]
async fn same_msg_id_is_deduplicated() {
    let (_c, url) = start_nats().await;
    let cfg = StreamConfig { num_replicas: 1, ..StreamConfig::default() };
    let client = NatsClient::connect(&url, &cfg).await.unwrap();
    let publisher = NatsPublisher::new(client);

    let evt = TestEvent { id: MessageId::new(), value: "hello".into() };

    let r1 = publisher.publish(&evt).await.unwrap();
    let r2 = publisher.publish(&evt).await.unwrap();

    assert!(!r1.duplicate);
    assert!(r2.duplicate, "second publish with same Nats-Msg-Id must be deduplicated");
    assert_eq!(r1.sequence, r2.sequence, "dedup must return same sequence");
}

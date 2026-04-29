use bus_core::{IdempotencyStore, MessageId};
use bus_nats::{NatsClient, NatsKvIdempotencyStore, StreamConfig};
use std::time::Duration;
use testcontainers::{
    core::{IntoContainerPort, WaitFor},
    runners::AsyncRunner,
    GenericImage, ImageExt,
};

async fn start_nats() -> (impl Drop, String) {
    let container = GenericImage::new("nats", "2.10-alpine")
        .with_exposed_port(4222.tcp())
        .with_wait_for(WaitFor::message_on_stderr("Server is ready"))
        .with_cmd(["-js"])
        .start()
        .await
        .unwrap();

    let host = container.get_host().await.unwrap();
    let port = container.get_host_port_ipv4(4222).await.unwrap();
    (container, format!("nats://{}:{}", host, port))
}

#[tokio::test]
async fn first_insert_returns_true() {
    let (_c, url) = start_nats().await;
    let cfg = StreamConfig { num_replicas: 1, ..Default::default() };
    let client = NatsClient::connect(&url, &cfg).await.unwrap();
    let store = NatsKvIdempotencyStore::new(client.jetstream().clone(), Duration::from_secs(60))
        .await
        .unwrap();

    let id = MessageId::new();
    let result = store.try_insert(&id, Duration::from_secs(60)).await.unwrap();
    assert!(result, "first insert must return true");
}

#[tokio::test]
async fn second_insert_returns_false() {
    let (_c, url) = start_nats().await;
    let cfg = StreamConfig { num_replicas: 1, ..Default::default() };
    let client = NatsClient::connect(&url, &cfg).await.unwrap();
    let store = NatsKvIdempotencyStore::new(client.jetstream().clone(), Duration::from_secs(60))
        .await
        .unwrap();

    let id = MessageId::new();
    let _ = store.try_insert(&id, Duration::from_secs(60)).await.unwrap();
    let result = store.try_insert(&id, Duration::from_secs(60)).await.unwrap();
    assert!(!result, "second insert with same key must return false");
}

#[tokio::test]
async fn mark_done_does_not_error() {
    let (_c, url) = start_nats().await;
    let cfg = StreamConfig { num_replicas: 1, ..Default::default() };
    let client = NatsClient::connect(&url, &cfg).await.unwrap();
    let store = NatsKvIdempotencyStore::new(client.jetstream().clone(), Duration::from_secs(60))
        .await
        .unwrap();

    let id = MessageId::new();
    store.try_insert(&id, Duration::from_secs(60)).await.unwrap();
    store.mark_done(&id).await.unwrap();
}

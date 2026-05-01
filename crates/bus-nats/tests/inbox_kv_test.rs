use bus_core::{ClaimOutcome, IdempotencyStore, MessageId};
use bus_nats::{NatsClient, NatsKvIdempotencyStore, StreamConfig};
use std::time::Duration;
use testcontainers::{
    ContainerAsync, GenericImage, ImageExt,
    core::{IntoContainerPort, WaitFor},
    runners::AsyncRunner,
};

async fn start_nats() -> (ContainerAsync<GenericImage>, String) {
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

async fn connect_client(url: &str) -> NatsClient {
    let cfg = StreamConfig {
        num_replicas: 1,
        ..Default::default()
    };
    NatsClient::connect(url, &cfg).await.unwrap()
}

#[tokio::test]
async fn first_claim_returns_claimed() {
    let (_c, url) = start_nats().await;
    let client = connect_client(&url).await;
    let store = NatsKvIdempotencyStore::new(client.jetstream().clone(), Duration::from_secs(60))
        .await
        .unwrap();

    let id = MessageId::new();
    let outcome = store.try_claim(&id, Duration::from_secs(60)).await.unwrap();
    assert_eq!(outcome, ClaimOutcome::Claimed);
}

#[tokio::test]
async fn second_claim_on_pending_returns_already_pending() {
    let (_c, url) = start_nats().await;
    let client = connect_client(&url).await;
    let store = NatsKvIdempotencyStore::new(client.jetstream().clone(), Duration::from_secs(60))
        .await
        .unwrap();

    let id = MessageId::new();
    store.try_claim(&id, Duration::from_secs(60)).await.unwrap();
    let outcome = store.try_claim(&id, Duration::from_secs(60)).await.unwrap();
    assert_eq!(outcome, ClaimOutcome::AlreadyPending);
}

#[tokio::test]
async fn claim_after_mark_done_returns_already_done() {
    let (_c, url) = start_nats().await;
    let client = connect_client(&url).await;
    let store = NatsKvIdempotencyStore::new(client.jetstream().clone(), Duration::from_secs(60))
        .await
        .unwrap();

    let id = MessageId::new();
    store.try_claim(&id, Duration::from_secs(60)).await.unwrap();
    store.mark_done(&id).await.unwrap();

    let outcome = store.try_claim(&id, Duration::from_secs(60)).await.unwrap();
    assert_eq!(outcome, ClaimOutcome::AlreadyDone);
}

#[tokio::test]
async fn claim_after_release_returns_claimed_again() {
    let (_c, url) = start_nats().await;
    let client = connect_client(&url).await;
    let store = NatsKvIdempotencyStore::new(client.jetstream().clone(), Duration::from_secs(60))
        .await
        .unwrap();

    let id = MessageId::new();
    store.try_claim(&id, Duration::from_secs(60)).await.unwrap();
    store.release(&id).await.unwrap();

    let outcome = store.try_claim(&id, Duration::from_secs(60)).await.unwrap();
    assert_eq!(outcome, ClaimOutcome::Claimed);
}

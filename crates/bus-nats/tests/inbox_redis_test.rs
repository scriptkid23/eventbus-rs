#![cfg(feature = "redis-inbox")]

use bus_core::{ClaimOutcome, IdempotencyStore, MessageId};
use bus_nats::{RedisIdempotencyConfig, RedisIdempotencyStore};
use std::{sync::Arc, time::Duration};
use testcontainers::{
    ContainerAsync, GenericImage,
    core::{IntoContainerPort, WaitFor},
    runners::AsyncRunner,
};

async fn start_redis() -> (ContainerAsync<GenericImage>, String) {
    let c = GenericImage::new("redis", "7-alpine")
        .with_exposed_port(6379.tcp())
        .with_wait_for(WaitFor::message_on_stdout("Ready to accept connections"))
        .start()
        .await
        .unwrap();
    let host = c.get_host().await.unwrap();
    let port = c.get_host_port_ipv4(6379).await.unwrap();
    (c, format!("redis://{host}:{port}"))
}

async fn connect_store(url: &str, ttl: Duration) -> RedisIdempotencyStore {
    RedisIdempotencyStore::connect(RedisIdempotencyConfig {
        url: url.into(),
        ttl,
        ..Default::default()
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn first_claim_returns_claimed() {
    let (_c, url) = start_redis().await;
    let store = connect_store(&url, Duration::from_secs(60)).await;
    let id = MessageId::new();
    assert_eq!(store.try_claim(&id).await.unwrap(), ClaimOutcome::Claimed);
}

#[tokio::test]
async fn second_claim_on_pending_returns_already_pending() {
    let (_c, url) = start_redis().await;
    let store = connect_store(&url, Duration::from_secs(60)).await;
    let id = MessageId::new();
    store.try_claim(&id).await.unwrap();
    assert_eq!(
        store.try_claim(&id).await.unwrap(),
        ClaimOutcome::AlreadyPending
    );
}

#[tokio::test]
async fn claim_after_mark_done_returns_already_done() {
    let (_c, url) = start_redis().await;
    let store = connect_store(&url, Duration::from_secs(60)).await;
    let id = MessageId::new();
    store.try_claim(&id).await.unwrap();
    store.mark_done(&id).await.unwrap();
    assert_eq!(
        store.try_claim(&id).await.unwrap(),
        ClaimOutcome::AlreadyDone
    );
}

#[tokio::test]
async fn claim_after_release_returns_claimed_again() {
    let (_c, url) = start_redis().await;
    let store = connect_store(&url, Duration::from_secs(60)).await;
    let id = MessageId::new();
    store.try_claim(&id).await.unwrap();
    store.release(&id).await.unwrap();
    assert_eq!(store.try_claim(&id).await.unwrap(), ClaimOutcome::Claimed);
}

#[tokio::test]
async fn fifty_concurrent_claims_yield_exactly_one_claimed() {
    let (_c, url) = start_redis().await;
    let store = Arc::new(connect_store(&url, Duration::from_secs(60)).await);
    let id = MessageId::new();

    let mut handles = Vec::new();
    for _ in 0..50 {
        let s = store.clone();
        let key = id.clone();
        handles.push(tokio::spawn(
            async move { s.try_claim(&key).await.unwrap() },
        ));
    }

    let mut claimed = 0;
    let mut pending = 0;
    for h in handles {
        match h.await.unwrap() {
            ClaimOutcome::Claimed => claimed += 1,
            ClaimOutcome::AlreadyPending => pending += 1,
            ClaimOutcome::AlreadyDone => panic!("unexpected AlreadyDone"),
        }
    }
    assert_eq!(claimed, 1);
    assert_eq!(pending, 49);
}

#[tokio::test]
async fn ttl_expiry_allows_reclaim() {
    let (_c, url) = start_redis().await;
    let store = connect_store(&url, Duration::from_secs(1)).await;
    let id = MessageId::new();
    store.try_claim(&id).await.unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(store.try_claim(&id).await.unwrap(), ClaimOutcome::Claimed);
}

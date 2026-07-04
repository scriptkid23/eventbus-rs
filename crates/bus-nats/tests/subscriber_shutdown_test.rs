use async_trait::async_trait;
use bus_core::{EventHandler, HandlerCtx, HandlerError, MessageId, Publisher};
use bus_nats::subscriber::subscribe;
use bus_nats::{
    NatsClient, NatsKvIdempotencyConfig, NatsKvIdempotencyStore, NatsPublisher, StreamConfig,
    SubscribeOptions,
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

#[derive(Debug, Serialize, Deserialize, Event)]
#[event(subject = "events.shutdown.test")]
struct ShutdownEvent {
    id: MessageId,
}

struct SlowHandler {
    started: Arc<AtomicU32>,
    completed: Arc<AtomicU32>,
}

#[async_trait]
impl EventHandler<ShutdownEvent> for SlowHandler {
    async fn handle(&self, _ctx: HandlerCtx, _evt: ShutdownEvent) -> Result<(), HandlerError> {
        self.started.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_secs(5)).await;
        self.completed.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn dropping_subscription_handle_aborts_in_flight_workers() {
    let (_container, url) = start_nats().await;
    let client = connect_client(&url).await;
    let publisher = NatsPublisher::new(client.clone());
    let store = Arc::new(
        NatsKvIdempotencyStore::new(
            client.jetstream().clone(),
            NatsKvIdempotencyConfig {
                num_replicas: 1,
                max_age: Duration::from_secs(60),
                ..Default::default()
            },
        )
        .await
        .unwrap(),
    );

    let started = Arc::new(AtomicU32::new(0));
    let completed = Arc::new(AtomicU32::new(0));

    let handle = subscribe::<ShutdownEvent, _, _>(
        client.clone(),
        SubscribeOptions {
            durable: "shutdown-test".into(),
            filter: "events.shutdown.>".into(),
            max_deliver: 1,
            backoff: vec![],
            concurrency: 4,
            ..Default::default()
        },
        Arc::new(SlowHandler {
            started: started.clone(),
            completed: completed.clone(),
        }),
        store,
    )
    .await
    .unwrap();

    publisher
        .publish(&ShutdownEvent {
            id: MessageId::new(),
        })
        .await
        .unwrap();
    publisher
        .publish(&ShutdownEvent {
            id: MessageId::new(),
        })
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        started.load(Ordering::SeqCst) >= 1,
        "handler should have started"
    );

    drop(handle);
    tokio::time::sleep(Duration::from_secs(6)).await;

    assert_eq!(
        completed.load(Ordering::SeqCst),
        0,
        "handler must not complete after SubscriptionHandle is dropped"
    );
}

struct DrainSlowHandler {
    started: Arc<AtomicU32>,
    finished: Arc<AtomicU32>,
}

#[async_trait]
impl EventHandler<ShutdownEvent> for DrainSlowHandler {
    async fn handle(&self, _ctx: HandlerCtx, _evt: ShutdownEvent) -> Result<(), HandlerError> {
        self.started.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(500)).await;
        self.finished.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn drain_waits_for_in_flight_handler() {
    let (_container, url) = start_nats().await;
    let client = connect_client(&url).await;
    let publisher = NatsPublisher::new(client.clone());
    let store = Arc::new(
        NatsKvIdempotencyStore::new(
            client.jetstream().clone(),
            NatsKvIdempotencyConfig {
                num_replicas: 1,
                max_age: Duration::from_secs(60),
                ..Default::default()
            },
        )
        .await
        .unwrap(),
    );

    let started = Arc::new(AtomicU32::new(0));
    let finished = Arc::new(AtomicU32::new(0));
    let handler = Arc::new(DrainSlowHandler {
        started: started.clone(),
        finished: finished.clone(),
    });

    let opts = SubscribeOptions {
        durable: "drain-worker".into(),
        filter: "events.shutdown.>".into(),
        ..Default::default()
    };

    let handle = subscribe::<ShutdownEvent, _, _>(client, opts, handler, store)
        .await
        .unwrap();

    publisher
        .publish(&ShutdownEvent {
            id: MessageId::new(),
        })
        .await
        .unwrap();

    // Wait until the handler has started but not finished.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(started.load(Ordering::SeqCst), 1);
    assert_eq!(finished.load(Ordering::SeqCst), 0);

    let drained = handle.drain(Duration::from_secs(5)).await;

    assert!(drained, "drain must complete within the timeout");
    assert_eq!(
        finished.load(Ordering::SeqCst),
        1,
        "in-flight handler must finish before drain returns"
    );
}

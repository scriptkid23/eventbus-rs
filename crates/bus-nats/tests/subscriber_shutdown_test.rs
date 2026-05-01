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
    started:   Arc<AtomicU32>,
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
        NatsKvIdempotencyStore::new(client.jetstream().clone(), Duration::from_secs(60))
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
            started:   started.clone(),
            completed: completed.clone(),
        }),
        store,
    )
    .await
    .unwrap();

    publisher
        .publish(&ShutdownEvent { id: MessageId::new() })
        .await
        .unwrap();
    publisher
        .publish(&ShutdownEvent { id: MessageId::new() })
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(started.load(Ordering::SeqCst) >= 1, "handler should have started");

    drop(handle);
    tokio::time::sleep(Duration::from_secs(6)).await;

    assert_eq!(
        completed.load(Ordering::SeqCst),
        0,
        "handler must not complete after SubscriptionHandle is dropped"
    );
}

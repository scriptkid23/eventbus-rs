use async_trait::async_trait;
use bus_core::{EventHandler, HandlerCtx, HandlerError, MessageId, Publisher};
use bus_nats::dlq::{DlqConfig, DlqOptions};
use bus_nats::subscriber::subscribe;
use bus_nats::{
    NatsClient, NatsKvIdempotencyConfig, NatsKvIdempotencyStore, NatsPublisher, StreamConfig,
    SubscribeOptions,
};
use eventbus_macros::Event;
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Duration};
use testcontainers::{
    GenericImage, ImageExt,
    core::{IntoContainerPort, WaitFor},
    runners::AsyncRunner,
};

#[derive(Debug, Serialize, Deserialize, Event)]
#[event(subject = "events.auto.dlq")]
struct E {
    id: MessageId,
}

struct AlwaysPermanent;

#[async_trait]
impl EventHandler<E> for AlwaysPermanent {
    async fn handle(&self, _: HandlerCtx, _: E) -> Result<(), HandlerError> {
        Err(HandlerError::Permanent("nope".into()))
    }
}

#[tokio::test]
async fn subscribe_auto_creates_dlq_stream_when_dlq_options_set() {
    let c = GenericImage::new("nats", "2.10-alpine")
        .with_exposed_port(4222.tcp())
        .with_wait_for(WaitFor::message_on_stderr("Server is ready"))
        .with_cmd(["-js"])
        .start()
        .await
        .unwrap();
    let host = c.get_host().await.unwrap();
    let port = c.get_host_port_ipv4(4222).await.unwrap();
    let url = format!("nats://{host}:{port}");

    let stream_cfg = StreamConfig {
        num_replicas: 1,
        ..Default::default()
    };
    let client = NatsClient::connect(&url, &stream_cfg).await.unwrap();
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
    let publisher = NatsPublisher::new(client.clone());

    let opts = SubscribeOptions {
        durable: "auto-dlq".into(),
        filter: "events.auto.>".into(),
        max_deliver: 2,
        ack_wait: Duration::from_secs(1),
        backoff: vec![Duration::from_millis(100)],
        dlq: Some(DlqOptions {
            config: DlqConfig {
                num_replicas: 1,
                ..Default::default()
            },
        }),
        ..Default::default()
    };

    let _h = subscribe::<E, _, _>(client.clone(), opts, Arc::new(AlwaysPermanent), store)
        .await
        .unwrap();

    publisher
        .publish(&E {
            id: MessageId::new(),
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;

    let dlq = client
        .jetstream()
        .get_stream("DLQ_EVENTS_auto-dlq")
        .await
        .unwrap();
    assert_eq!(dlq.get_info().await.unwrap().state.messages, 1);
}

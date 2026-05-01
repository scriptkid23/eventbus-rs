use async_nats::jetstream;
use async_trait::async_trait;
use bus_core::{EventHandler, HandlerCtx, HandlerError, MessageId, Publisher};
use bus_macros::Event;
use bus_nats::dlq::{
    DEFAULT_DLQ_DUPLICATE_WINDOW, DEFAULT_DLQ_MAX_AGE, DEFAULT_DLQ_REPLICAS, DlqConfig, DlqOptions,
    FailureInfo, build_dlq_headers, dlq_stream_name, dlq_subject, ensure_dlq_stream,
    publish_to_dlq,
};
use bus_nats::subscriber::subscribe;
use bus_nats::{NatsClient, NatsKvIdempotencyStore, NatsPublisher, StreamConfig, SubscribeOptions};
use bytes::Bytes;
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

async fn connect_jetstream(url: &str) -> jetstream::Context {
    let mut last_error = None;

    for _ in 0..20 {
        match async_nats::connect(url).await {
            Ok(client) => return jetstream::new(client),
            Err(error) => {
                last_error = Some(error);
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }

    panic!("failed to connect to NATS: {:?}", last_error);
}

async fn connect_nats_client(url: &str) -> NatsClient {
    let config = StreamConfig {
        num_replicas: 1,
        ..Default::default()
    };
    let mut last_error = None;

    for _ in 0..20 {
        match NatsClient::connect(url, &config).await {
            Ok(client) => return client,
            Err(error) => {
                last_error = Some(error);
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }

    panic!("failed to connect NatsClient: {:?}", last_error);
}

#[derive(Debug, Serialize, Deserialize, Event)]
#[event(subject = "events.dlq.test")]
struct DlqTestEvent {
    id: MessageId,
    value: u32,
}

struct AlwaysPermanent;

#[async_trait]
impl EventHandler<DlqTestEvent> for AlwaysPermanent {
    async fn handle(&self, _ctx: HandlerCtx, _evt: DlqTestEvent) -> Result<(), HandlerError> {
        Err(HandlerError::Permanent("invalid card".into()))
    }
}

struct AlwaysTransient(Arc<AtomicU32>);

#[async_trait]
impl EventHandler<DlqTestEvent> for AlwaysTransient {
    async fn handle(&self, _ctx: HandlerCtx, _evt: DlqTestEvent) -> Result<(), HandlerError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(HandlerError::Transient("db down".into()))
    }
}

struct TimingHandler(Arc<tokio::sync::Mutex<Vec<std::time::Instant>>>);

#[async_trait]
impl EventHandler<DlqTestEvent> for TimingHandler {
    async fn handle(&self, _ctx: HandlerCtx, _evt: DlqTestEvent) -> Result<(), HandlerError> {
        self.0.lock().await.push(std::time::Instant::now());
        Err(HandlerError::Transient("retrying".into()))
    }
}

struct CountingPermanent(Arc<AtomicU32>);

#[async_trait]
impl EventHandler<DlqTestEvent> for CountingPermanent {
    async fn handle(&self, _ctx: HandlerCtx, _evt: DlqTestEvent) -> Result<(), HandlerError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(HandlerError::Permanent("always fails".into()))
    }
}

#[test]
fn dlq_stream_name_uses_source_and_durable() {
    assert_eq!(dlq_stream_name("EVENTS", "billing"), "DLQ_EVENTS_billing");
    assert_eq!(
        dlq_stream_name("ORDERS", "fulfillment-worker"),
        "DLQ_ORDERS_fulfillment-worker"
    );
}

#[test]
fn dlq_subject_uses_lowercase_dot_pattern() {
    assert_eq!(dlq_subject("EVENTS", "billing"), "dlq.EVENTS.billing");
}

#[test]
fn dlq_config_defaults_are_production_safe() {
    let config = DlqConfig::default();

    assert_eq!(config.num_replicas, DEFAULT_DLQ_REPLICAS);
    assert_eq!(config.max_age, DEFAULT_DLQ_MAX_AGE);
    assert_eq!(config.duplicate_window, DEFAULT_DLQ_DUPLICATE_WINDOW);
    assert!(config.deny_delete);
    assert!(!config.deny_purge);
    assert!(config.allow_direct);
}

#[test]
fn dlq_options_inherits_config_defaults() {
    let options = DlqOptions::default();

    assert_eq!(options.config.num_replicas, DEFAULT_DLQ_REPLICAS);
}

#[tokio::test]
async fn ensure_dlq_stream_creates_stream_with_correct_config() {
    let (_container, url) = start_nats().await;
    let js = connect_jetstream(&url).await;
    let config = DlqConfig {
        num_replicas: 1,
        ..Default::default()
    };

    ensure_dlq_stream(&js, "DLQ_TEST_worker", "dlq.TEST.worker", &config)
        .await
        .unwrap();

    let info = js
        .get_stream("DLQ_TEST_worker")
        .await
        .unwrap()
        .get_info()
        .await
        .unwrap();
    assert_eq!(info.config.subjects, vec!["dlq.TEST.worker".to_string()]);
    assert_eq!(info.config.num_replicas, 1);
    assert_eq!(info.config.max_age, config.max_age);
    assert!(info.config.deny_delete);
    assert!(info.config.allow_direct);
}

#[tokio::test]
async fn ensure_dlq_stream_is_idempotent() {
    let (_container, url) = start_nats().await;
    let js = connect_jetstream(&url).await;
    let config = DlqConfig {
        num_replicas: 1,
        ..Default::default()
    };

    ensure_dlq_stream(&js, "DLQ_X_y", "dlq.X.y", &config)
        .await
        .unwrap();
    ensure_dlq_stream(&js, "DLQ_X_y", "dlq.X.y", &config)
        .await
        .unwrap();
}

#[test]
fn build_dlq_headers_includes_required_fields() {
    let failure_info = FailureInfo {
        original_subject: "events.order.created".into(),
        original_stream: "EVENTS".into(),
        original_seq: 42,
        original_msg_id: "01HW...".into(),
        consumer: "billing".into(),
        delivered: 5,
        failure_reason: "handler_permanent".into(),
        failure_class: "permanent".into(),
        failure_detail: "invalid card number".into(),
    };

    let headers = build_dlq_headers(&failure_info);

    assert_eq!(
        headers.get("X-Original-Subject").unwrap().as_str(),
        "events.order.created"
    );
    assert_eq!(headers.get("X-Original-Stream").unwrap().as_str(), "EVENTS");
    assert_eq!(headers.get("X-Original-Seq").unwrap().as_str(), "42");
    assert_eq!(
        headers.get("X-Original-Msg-Id").unwrap().as_str(),
        "01HW..."
    );
    assert_eq!(headers.get("X-Consumer").unwrap().as_str(), "billing");
    assert_eq!(headers.get("X-Retry-Count").unwrap().as_str(), "5");
    assert_eq!(
        headers.get("X-Failure-Reason").unwrap().as_str(),
        "handler_permanent"
    );
    assert_eq!(
        headers.get("X-Failure-Class").unwrap().as_str(),
        "permanent"
    );
    assert_eq!(
        headers.get("X-Failure-Detail").unwrap().as_str(),
        "invalid card number"
    );
    assert!(headers.get("X-Failed-At").is_some());
}

#[test]
fn build_dlq_headers_sets_deterministic_dedup_id() {
    let failure_info = FailureInfo {
        original_subject: "events.x".into(),
        original_stream: "EVENTS".into(),
        original_seq: 7,
        original_msg_id: "abc".into(),
        consumer: "w".into(),
        delivered: 3,
        failure_reason: "permanent".into(),
        failure_class: "permanent".into(),
        failure_detail: String::new(),
    };

    let headers_a = build_dlq_headers(&failure_info);
    let headers_b = build_dlq_headers(&failure_info);

    let dedup_id_a = headers_a.get("Nats-Msg-Id").unwrap().as_str();
    let dedup_id_b = headers_b.get("Nats-Msg-Id").unwrap().as_str();
    assert_eq!(dedup_id_a, dedup_id_b);
    assert_eq!(dedup_id_a, "EVENTS:7:w:3");
}

#[tokio::test]
async fn publish_to_dlq_persists_payload_and_headers() {
    let (_container, url) = start_nats().await;
    let js = connect_jetstream(&url).await;
    let config = DlqConfig {
        num_replicas: 1,
        ..Default::default()
    };

    ensure_dlq_stream(&js, "DLQ_X_w", "dlq.X.w", &config)
        .await
        .unwrap();

    let failure_info = FailureInfo {
        original_subject: "events.x".into(),
        original_stream: "X".into(),
        original_seq: 1,
        original_msg_id: "m1".into(),
        consumer: "w".into(),
        delivered: 5,
        failure_reason: "permanent".into(),
        failure_class: "permanent".into(),
        failure_detail: "boom".into(),
    };
    let payload = Bytes::from_static(br#"{"hello":"world"}"#);

    publish_to_dlq(
        &js,
        "dlq.X.w",
        payload.clone(),
        build_dlq_headers(&failure_info),
    )
    .await
    .unwrap();

    let stream = js.get_stream("DLQ_X_w").await.unwrap();
    let raw = stream
        .direct_get_first_for_subject("dlq.X.w")
        .await
        .unwrap();
    assert_eq!(raw.payload, payload);
    assert_eq!(
        raw.headers.get("X-Failure-Reason").unwrap().as_str(),
        "permanent"
    );
}

#[tokio::test]
async fn publish_to_dlq_returns_err_when_no_stream_captures_subject() {
    let (_container, url) = start_nats().await;
    let js = connect_jetstream(&url).await;
    let mut headers = async_nats::HeaderMap::new();
    headers.insert("Nats-Msg-Id", "test-id");

    let result = publish_to_dlq(&js, "dlq.NONEXISTENT.x", Bytes::from_static(b"x"), headers).await;

    assert!(result.is_err());
}

#[tokio::test]
async fn publish_to_dlq_dedups_via_nats_msg_id() {
    let (_container, url) = start_nats().await;
    let js = connect_jetstream(&url).await;
    let config = DlqConfig {
        num_replicas: 1,
        ..Default::default()
    };

    ensure_dlq_stream(&js, "DLQ_X_w", "dlq.X.w", &config)
        .await
        .unwrap();

    let failure_info = FailureInfo {
        original_subject: "events.x".into(),
        original_stream: "X".into(),
        original_seq: 1,
        original_msg_id: "m1".into(),
        consumer: "w".into(),
        delivered: 5,
        failure_reason: "permanent".into(),
        failure_class: "permanent".into(),
        failure_detail: String::new(),
    };

    publish_to_dlq(
        &js,
        "dlq.X.w",
        Bytes::from_static(b"first"),
        build_dlq_headers(&failure_info),
    )
    .await
    .unwrap();
    publish_to_dlq(
        &js,
        "dlq.X.w",
        Bytes::from_static(b"second"),
        build_dlq_headers(&failure_info),
    )
    .await
    .unwrap();

    let stream = js.get_stream("DLQ_X_w").await.unwrap();
    let info = stream.get_info().await.unwrap();
    assert_eq!(info.state.messages, 1);
}

#[tokio::test]
async fn permanent_error_publishes_to_dlq_with_enriched_headers() {
    let (_container, url) = start_nats().await;
    let client = connect_nats_client(&url).await;
    let publisher = NatsPublisher::new(client.clone());
    let store = Arc::new(
        NatsKvIdempotencyStore::new(client.jetstream().clone(), Duration::from_secs(60))
            .await
            .unwrap(),
    );
    let dlq_config = DlqConfig {
        num_replicas: 1,
        ..Default::default()
    };

    ensure_dlq_stream(
        client.jetstream(),
        "DLQ_EVENTS_dlq-test",
        "dlq.EVENTS.dlq-test",
        &dlq_config,
    )
    .await
    .unwrap();

    let options = SubscribeOptions {
        durable: "dlq-test".into(),
        filter: "events.dlq.>".into(),
        max_deliver: 3,
        backoff: vec![Duration::from_millis(100), Duration::from_millis(200)],
        dlq: Some(DlqOptions {
            config: dlq_config.clone(),
        }),
        ..Default::default()
    };

    let _handle =
        subscribe::<DlqTestEvent, _, _>(client.clone(), options, Arc::new(AlwaysPermanent), store)
            .await
            .unwrap();

    let event = DlqTestEvent {
        id: MessageId::new(),
        value: 1,
    };
    publisher.publish(&event).await.unwrap();

    tokio::time::sleep(Duration::from_secs(2)).await;

    let dlq_stream = client
        .jetstream()
        .get_stream("DLQ_EVENTS_dlq-test")
        .await
        .unwrap();
    let info = dlq_stream.get_info().await.unwrap();
    assert_eq!(info.state.messages, 1);

    let raw = dlq_stream
        .direct_get_first_for_subject("dlq.EVENTS.dlq-test")
        .await
        .unwrap();
    let headers = raw.headers;
    assert_eq!(
        headers.get("X-Failure-Class").unwrap().as_str(),
        "permanent"
    );
    assert_eq!(
        headers.get("X-Failure-Reason").unwrap().as_str(),
        "handler_permanent"
    );
    assert_eq!(
        headers.get("X-Failure-Detail").unwrap().as_str(),
        "invalid card"
    );
    assert_eq!(headers.get("X-Consumer").unwrap().as_str(), "dlq-test");
    assert_eq!(headers.get("X-Original-Stream").unwrap().as_str(), "EVENTS");
}

#[tokio::test]
async fn deserialize_failure_publishes_to_dlq_as_poison() {
    let (_container, url) = start_nats().await;
    let client = connect_nats_client(&url).await;
    let store = Arc::new(
        NatsKvIdempotencyStore::new(client.jetstream().clone(), Duration::from_secs(60))
            .await
            .unwrap(),
    );
    let dlq_config = DlqConfig {
        num_replicas: 1,
        ..Default::default()
    };

    ensure_dlq_stream(
        client.jetstream(),
        "DLQ_EVENTS_poison-test",
        "dlq.EVENTS.poison-test",
        &dlq_config,
    )
    .await
    .unwrap();

    let options = SubscribeOptions {
        durable: "poison-test".into(),
        filter: "events.poison.>".into(),
        max_deliver: 3,
        backoff: vec![Duration::from_millis(100), Duration::from_millis(200)],
        dlq: Some(DlqOptions { config: dlq_config }),
        ..Default::default()
    };

    let _handle =
        subscribe::<DlqTestEvent, _, _>(client.clone(), options, Arc::new(AlwaysPermanent), store)
            .await
            .unwrap();

    let js = connect_jetstream(&url).await;
    let mut headers = async_nats::HeaderMap::new();
    headers.insert("Nats-Msg-Id", MessageId::new().to_string().as_str());
    let publish_ack = js
        .publish_with_headers("events.poison.x", headers, Bytes::from_static(b"NOT JSON"))
        .await
        .unwrap();
    publish_ack.await.unwrap();

    tokio::time::sleep(Duration::from_secs(2)).await;

    let dlq_stream = client
        .jetstream()
        .get_stream("DLQ_EVENTS_poison-test")
        .await
        .unwrap();
    let info = dlq_stream.get_info().await.unwrap();
    assert_eq!(info.state.messages, 1);

    let raw = dlq_stream
        .direct_get_first_for_subject("dlq.EVENTS.poison-test")
        .await
        .unwrap();
    let headers = raw.headers;
    assert_eq!(headers.get("X-Failure-Class").unwrap().as_str(), "poison");
    assert_eq!(
        headers.get("X-Failure-Reason").unwrap().as_str(),
        "invalid_payload"
    );
    assert_eq!(raw.payload.as_ref(), b"NOT JSON");
}

#[tokio::test]
async fn transient_exhausting_max_deliver_publishes_to_dlq() {
    let (_container, url) = start_nats().await;
    let client = connect_nats_client(&url).await;
    let publisher = NatsPublisher::new(client.clone());
    let store = Arc::new(
        NatsKvIdempotencyStore::new(client.jetstream().clone(), Duration::from_secs(60))
            .await
            .unwrap(),
    );
    let dlq_config = DlqConfig {
        num_replicas: 1,
        ..Default::default()
    };

    ensure_dlq_stream(
        client.jetstream(),
        "DLQ_EVENTS_transient-test",
        "dlq.EVENTS.transient-test",
        &dlq_config,
    )
    .await
    .unwrap();

    let counter = Arc::new(AtomicU32::new(0));
    let options = SubscribeOptions {
        durable: "transient-test".into(),
        filter: "events.dlq.>".into(),
        max_deliver: 3,
        ack_wait: Duration::from_secs(1),
        backoff: vec![Duration::from_millis(100), Duration::from_millis(100)],
        dlq: Some(DlqOptions { config: dlq_config }),
        ..Default::default()
    };

    let _handle = subscribe::<DlqTestEvent, _, _>(
        client.clone(),
        options,
        Arc::new(AlwaysTransient(counter.clone())),
        store,
    )
    .await
    .unwrap();

    publisher
        .publish(&DlqTestEvent {
            id: MessageId::new(),
            value: 1,
        })
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_secs(3)).await;

    assert_eq!(counter.load(Ordering::SeqCst), 3);

    let dlq_stream = client
        .jetstream()
        .get_stream("DLQ_EVENTS_transient-test")
        .await
        .unwrap();
    let info = dlq_stream.get_info().await.unwrap();
    assert_eq!(info.state.messages, 1);

    let raw = dlq_stream
        .direct_get_first_for_subject("dlq.EVENTS.transient-test")
        .await
        .unwrap();
    let headers = raw.headers;
    assert_eq!(
        headers.get("X-Failure-Class").unwrap().as_str(),
        "transient_exhausted"
    );
    assert_eq!(
        headers.get("X-Failure-Reason").unwrap().as_str(),
        "max_retries_exceeded"
    );
    assert_eq!(headers.get("X-Retry-Count").unwrap().as_str(), "3");
}

#[tokio::test]
async fn transient_nak_uses_configured_backoff() {
    let (_container, url) = start_nats().await;
    let client = connect_nats_client(&url).await;
    let publisher = NatsPublisher::new(client.clone());
    let store = Arc::new(
        NatsKvIdempotencyStore::new(client.jetstream().clone(), Duration::from_secs(60))
            .await
            .unwrap(),
    );
    let dlq_config = DlqConfig {
        num_replicas: 1,
        ..Default::default()
    };

    ensure_dlq_stream(
        client.jetstream(),
        "DLQ_EVENTS_backoff-test",
        "dlq.EVENTS.backoff-test",
        &dlq_config,
    )
    .await
    .unwrap();

    let timestamps = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let options = SubscribeOptions {
        durable: "backoff-test".into(),
        filter: "events.dlq.>".into(),
        max_deliver: 3,
        ack_wait: Duration::from_secs(10),
        backoff: vec![Duration::from_millis(200), Duration::from_millis(800)],
        dlq: Some(DlqOptions { config: dlq_config }),
        ..Default::default()
    };

    let _handle = subscribe::<DlqTestEvent, _, _>(
        client.clone(),
        options,
        Arc::new(TimingHandler(timestamps.clone())),
        store,
    )
    .await
    .unwrap();

    publisher
        .publish(&DlqTestEvent {
            id: MessageId::new(),
            value: 1,
        })
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_secs(3)).await;

    let timestamps = timestamps.lock().await;
    assert_eq!(timestamps.len(), 3);

    let gap_one = timestamps[1].duration_since(timestamps[0]);
    let gap_two = timestamps[2].duration_since(timestamps[1]);
    assert!(gap_one >= Duration::from_millis(150), "gap: {gap_one:?}");
    assert!(gap_one < Duration::from_millis(600), "gap: {gap_one:?}");
    assert!(gap_two >= Duration::from_millis(700), "gap: {gap_two:?}");
}

#[tokio::test]
async fn dlq_publish_failure_naks_original_handler_invoked_max_deliver_times() {
    let (_container, url) = start_nats().await;
    let client = connect_nats_client(&url).await;
    let publisher = NatsPublisher::new(client.clone());
    let store = Arc::new(
        NatsKvIdempotencyStore::new(client.jetstream().clone(), Duration::from_secs(60))
            .await
            .unwrap(),
    );

    let counter = Arc::new(AtomicU32::new(0));
    let options = SubscribeOptions {
        durable: "no-dlq-stream".into(),
        filter: "events.dlq.>".into(),
        max_deliver: 3,
        ack_wait: Duration::from_secs(1),
        backoff: vec![Duration::from_millis(100), Duration::from_millis(100)],
        dlq: Some(DlqOptions {
            config: DlqConfig {
                num_replicas: 1,
                ..Default::default()
            },
        }),
        ..Default::default()
    };

    let _handle = subscribe::<DlqTestEvent, _, _>(
        client.clone(),
        options,
        Arc::new(CountingPermanent(counter.clone())),
        store,
    )
    .await
    .unwrap();

    publisher
        .publish(&DlqTestEvent {
            id: MessageId::new(),
            value: 1,
        })
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_secs(14)).await;

    assert_eq!(counter.load(Ordering::SeqCst), 3);
}

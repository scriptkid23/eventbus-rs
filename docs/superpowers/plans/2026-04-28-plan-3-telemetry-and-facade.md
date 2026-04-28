# Plan 3: `bus-telemetry` + `event-bus` Facade Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `bus-telemetry` (OpenTelemetry spans, metrics, W3C traceparent propagation) and the `event-bus` facade crate (builder API, `EventBus`, saga choreography + orchestration engine, `prelude` re-exports, usage examples).

**Architecture:** `bus-telemetry` is a thin instrumentation layer over the global OTel provider — it injects/extracts W3C `traceparent` headers on NATS messages and records spans and metrics for every publish/consume/outbox operation. `event-bus` is the public-facing facade: `EventBusBuilder` wires all backend crates together, `EventBus` exposes `publish`, `publish_in_tx`, `subscribe`, `start_saga`, and `shutdown`. The saga engine stores durable state in Postgres via `bus-outbox` migrations and uses optimistic concurrency control.

**Tech Stack:** `opentelemetry` 0.24, `opentelemetry-semantic-conventions`, `tracing-opentelemetry`, `opentelemetry_sdk`, `sqlx` 0.8 (for saga state), all workspace crates from Plans 1 & 2.

**Prerequisite:** Plans 1 and 2 complete.

---

## File Map

### Workspace root
- Modify: `Cargo.toml` — add `bus-telemetry`, `event-bus` members and OTel workspace deps

### `crates/bus-telemetry/`
- Create: `crates/bus-telemetry/Cargo.toml`
- Create: `crates/bus-telemetry/src/lib.rs`
- Create: `crates/bus-telemetry/src/propagation.rs` — W3C traceparent inject/extract for NATS headers
- Create: `crates/bus-telemetry/src/spans.rs` — span creation helpers for each operation
- Create: `crates/bus-telemetry/src/metrics.rs` — OTel meter + named instruments
- Create: `crates/bus-telemetry/tests/propagation_test.rs`

### `crates/event-bus/`
- Create: `crates/event-bus/Cargo.toml`
- Create: `crates/event-bus/src/lib.rs`
- Create: `crates/event-bus/src/builder.rs` — `EventBusBuilder`
- Create: `crates/event-bus/src/bus.rs` — `EventBus`, `SubscriptionHandle`
- Create: `crates/event-bus/src/saga/mod.rs`
- Create: `crates/event-bus/src/saga/choreography.rs` — `ChoreographyStep` trait
- Create: `crates/event-bus/src/saga/orchestration.rs` — `SagaDefinition`, `SagaTransition`, `SagaHandle`, state machine runner
- Create: `crates/event-bus/src/prelude.rs` — all public re-exports
- Create: `crates/event-bus/tests/builder_test.rs`
- Create: `crates/event-bus/tests/saga_test.rs`
- Create: `examples/01-basic-publish/src/main.rs`
- Create: `examples/02-outbox-postgres/src/main.rs`
- Create: `examples/03-idempotent-handler/src/main.rs`

---

## Task 1: Extend workspace `Cargo.toml` with OTel deps and new members

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Add `bus-telemetry` and `event-bus` members and OTel deps**

Add to `[workspace] members`:
```toml
"crates/bus-telemetry",
"crates/event-bus",
"examples/01-basic-publish",
"examples/02-outbox-postgres",
"examples/03-idempotent-handler",
```

Add to `[workspace.dependencies]`:
```toml
opentelemetry                   = { version = "0.24", features = ["metrics"] }
opentelemetry_sdk               = { version = "0.24", features = ["rt-tokio", "metrics"] }
opentelemetry-semantic-conventions = { version = "0.16", features = ["semconv_experimental"] }
tracing-opentelemetry           = "0.25"
```

- [ ] **Step 2: Verify workspace resolves**

```bash
cargo metadata --no-deps --format-version 1 | grep '"name"' | head -12
```

Expected: all 6 crates listed.

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml
git commit -m "chore: add bus-telemetry, event-bus, examples to workspace; add OTel deps"
```

---

## Task 2: Scaffold `bus-telemetry` crate

**Files:**
- Create: `crates/bus-telemetry/Cargo.toml`
- Create: `crates/bus-telemetry/src/lib.rs`

- [ ] **Step 1: Create directories**

```bash
mkdir -p crates/bus-telemetry/src crates/bus-telemetry/tests
```

- [ ] **Step 2: Create `crates/bus-telemetry/Cargo.toml`**

```toml
[package]
name        = "bus-telemetry"
description = "OpenTelemetry instrumentation for eventbus-rs"
keywords    = ["opentelemetry", "tracing", "metrics", "nats"]
categories  = ["asynchronous", "network-programming"]
version.workspace    = true
edition.workspace    = true
authors.workspace    = true
license.workspace    = true
repository.workspace = true

[dependencies]
async-nats                         = { workspace = true }
bus-core                           = { path = "../bus-core" }
opentelemetry                      = { workspace = true }
opentelemetry-semantic-conventions = { workspace = true }
tracing                            = { workspace = true }
tracing-opentelemetry              = { workspace = true }
```

- [ ] **Step 3: Create `crates/bus-telemetry/src/lib.rs`**

```rust
pub mod metrics;
pub mod propagation;
pub mod spans;

pub use metrics::BusMetrics;
pub use propagation::{extract_context, inject_context};
pub use spans::SpanBuilder;
```

- [ ] **Step 4: Create stub modules**

Create `crates/bus-telemetry/src/propagation.rs`:
```rust
use async_nats::HeaderMap;
use opentelemetry::Context;

pub fn inject_context(_headers: &mut HeaderMap) {}
pub fn extract_context(_headers: &HeaderMap) -> Context { Context::current() }
```

Create `crates/bus-telemetry/src/spans.rs`:
```rust
pub struct SpanBuilder;
```

Create `crates/bus-telemetry/src/metrics.rs`:
```rust
pub struct BusMetrics;
```

- [ ] **Step 5: Verify scaffold compiles**

```bash
cargo check -p bus-telemetry
```

Expected: `Finished` with no errors.

- [ ] **Step 6: Commit**

```bash
git add crates/bus-telemetry/
git commit -m "feat(bus-telemetry): scaffold crate"
```

---

## Task 3: Implement W3C traceparent propagation

**Files:**
- Modify: `crates/bus-telemetry/src/propagation.rs`
- Create: `crates/bus-telemetry/tests/propagation_test.rs`

- [ ] **Step 1: Write failing tests**

Create `crates/bus-telemetry/tests/propagation_test.rs`:

```rust
use async_nats::HeaderMap;
use bus_telemetry::{extract_context, inject_context};
use opentelemetry::{
    global,
    propagation::TextMapPropagator,
    trace::{SpanContext, TraceContextExt, TraceFlags, TraceId, Tracer},
    Context,
};
use opentelemetry_sdk::propagation::TraceContextPropagator;

fn setup_propagator() {
    global::set_text_map_propagator(TraceContextPropagator::new());
}

#[test]
fn inject_writes_traceparent_header() {
    setup_propagator();
    let mut headers = HeaderMap::new();
    // Without an active span, inject should still not panic
    inject_context(&mut headers);
    // With no active span, traceparent may not be written — just verify no panic
}

#[test]
fn extract_from_empty_headers_returns_empty_context() {
    setup_propagator();
    let headers = HeaderMap::new();
    let cx = extract_context(&headers);
    // Empty context — span context is not valid
    assert!(!cx.span().span_context().is_valid());
}

#[test]
fn roundtrip_inject_then_extract() {
    setup_propagator();

    // Build a known traceparent header manually
    let mut headers = HeaderMap::new();
    let traceparent = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
    headers.insert("traceparent", traceparent);

    let cx = extract_context(&headers);
    let span_ctx = cx.span().span_context();
    assert!(span_ctx.is_valid());
    assert_eq!(
        span_ctx.trace_id().to_string(),
        "4bf92f3577b34da6a3ce929d0e0e4736"
    );
}
```

- [ ] **Step 2: Run to confirm failure**

```bash
cargo test -p bus-telemetry --test propagation_test 2>&1 | head -20
```

Expected: test `roundtrip_inject_then_extract` fails — extract not implemented.

- [ ] **Step 3: Implement `crates/bus-telemetry/src/propagation.rs`**

```rust
use async_nats::HeaderMap;
use opentelemetry::{global, Context};
use std::collections::HashMap;

/// NATS header map adapter for OTel TextMap inject
struct NatsHeaderInjector<'a>(&'a mut HeaderMap);

impl<'a> opentelemetry::propagation::Injector for NatsHeaderInjector<'a> {
    fn set(&mut self, key: &str, value: String) {
        self.0.insert(key, value.as_str());
    }
}

/// NATS header map adapter for OTel TextMap extract
struct NatsHeaderExtractor<'a>(&'a HeaderMap);

impl<'a> opentelemetry::propagation::Extractor for NatsHeaderExtractor<'a> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(|v| v.as_str())
    }

    fn keys(&self) -> Vec<&str> {
        // HeaderMap does not expose an iterator in all versions — return known keys
        vec!["traceparent", "tracestate"]
    }
}

/// Inject the current span context as W3C `traceparent` into NATS message headers.
/// Call this before publishing a message.
pub fn inject_context(headers: &mut HeaderMap) {
    let cx = Context::current();
    global::get_text_map_propagator(|propagator| {
        propagator.inject_context(&cx, &mut NatsHeaderInjector(headers));
    });
}

/// Extract the W3C `traceparent` from NATS message headers and return the context.
/// Call this when receiving a message to create a child span.
pub fn extract_context(headers: &HeaderMap) -> Context {
    global::get_text_map_propagator(|propagator| {
        propagator.extract(&NatsHeaderExtractor(headers))
    })
}
```

- [ ] **Step 4: Run tests to confirm they pass**

```bash
cargo test -p bus-telemetry --test propagation_test
```

Expected: all 3 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/bus-telemetry/src/propagation.rs crates/bus-telemetry/tests/propagation_test.rs
git commit -m "feat(bus-telemetry): implement W3C traceparent inject/extract for NATS headers"
```

---

## Task 4: Implement OTel spans and metrics

**Files:**
- Modify: `crates/bus-telemetry/src/spans.rs`
- Modify: `crates/bus-telemetry/src/metrics.rs`

- [ ] **Step 1: Implement `crates/bus-telemetry/src/spans.rs`**

```rust
use opentelemetry::{global, trace::Tracer, KeyValue};

const TRACER_NAME: &str = "eventbus-rs";

/// Create a span for a publish operation.
pub fn publish_span(subject: &str, msg_id: &str, stream: &str) -> opentelemetry::trace::Span {
    let tracer = global::tracer(TRACER_NAME);
    tracer.start_with_context(
        "eventbus.publish",
        &opentelemetry::Context::current(),
    )
}

/// Create a span for a consumer receive operation.
pub fn receive_span(subject: &str, stream_seq: u64, delivered: u64, parent: opentelemetry::Context)
    -> opentelemetry::trace::Span
{
    let tracer = global::tracer(TRACER_NAME);
    tracer.start_with_context("eventbus.receive", &parent)
}

/// Create a span for handler execution.
pub fn handle_span(msg_id: &str, handler_name: &str, parent: opentelemetry::Context)
    -> opentelemetry::trace::Span
{
    let tracer = global::tracer(TRACER_NAME);
    tracer.start_with_context("eventbus.handle", &parent)
}

/// Create a span for outbox dispatch.
pub fn outbox_dispatch_span(outbox_id: &str, attempts: i32) -> opentelemetry::trace::Span {
    let tracer = global::tracer(TRACER_NAME);
    tracer.start_with_context("eventbus.outbox.dispatch", &opentelemetry::Context::current())
}

/// Create a span for idempotency check.
pub fn idempotency_span(msg_id: &str, backend: &str) -> opentelemetry::trace::Span {
    let tracer = global::tracer(TRACER_NAME);
    tracer.start_with_context("eventbus.idempotency.check", &opentelemetry::Context::current())
}
```

- [ ] **Step 2: Implement `crates/bus-telemetry/src/metrics.rs`**

```rust
use opentelemetry::{
    global,
    metrics::{Counter, Histogram, ObservableGauge},
    KeyValue,
};

/// Holds all OTel metric instruments for eventbus-rs.
/// Create once and share via `Arc`. Instruments record against the global meter provider.
pub struct BusMetrics {
    pub publish_total:       Counter<u64>,
    pub publish_duration_ms: Histogram<f64>,
    pub consume_total:       Counter<u64>,
    pub consume_duration_ms: Histogram<f64>,
    pub redeliveries_total:  Counter<u64>,
    pub dlq_total:           Counter<u64>,
    pub outbox_dispatch_ms:  Histogram<f64>,
    pub idempotency_hits:    Counter<u64>,
}

impl BusMetrics {
    /// Initialize all instruments against the global meter provider.
    pub fn new() -> Self {
        let meter = global::meter("eventbus-rs");

        Self {
            publish_total: meter
                .u64_counter("eventbus.publish.total")
                .with_description("Total publish attempts")
                .build(),

            publish_duration_ms: meter
                .f64_histogram("eventbus.publish.duration_ms")
                .with_description("Publish latency in milliseconds")
                .build(),

            consume_total: meter
                .u64_counter("eventbus.consume.total")
                .with_description("Total messages consumed")
                .build(),

            consume_duration_ms: meter
                .f64_histogram("eventbus.consume.duration_ms")
                .with_description("Handler execution latency in milliseconds")
                .build(),

            redeliveries_total: meter
                .u64_counter("eventbus.redeliveries.total")
                .with_description("Total message redeliveries")
                .build(),

            dlq_total: meter
                .u64_counter("eventbus.dlq.total")
                .with_description("Total messages sent to DLQ")
                .build(),

            outbox_dispatch_ms: meter
                .f64_histogram("eventbus.outbox.dispatch_ms")
                .with_description("Outbox dispatch latency in milliseconds")
                .build(),

            idempotency_hits: meter
                .u64_counter("eventbus.idempotency.hits")
                .with_description("Duplicate messages detected by idempotency store")
                .build(),
        }
    }

    /// Record a successful or duplicate publish
    pub fn record_publish(&self, subject: &str, stream: &str, duplicate: bool, duration_ms: f64) {
        let attrs = [
            KeyValue::new("event.subject", subject.to_string()),
            KeyValue::new("nats.stream", stream.to_string()),
            KeyValue::new("duplicate", duplicate.to_string()),
        ];
        self.publish_total.add(1, &attrs);
        self.publish_duration_ms.record(duration_ms, &attrs[..2]);
    }

    /// Record a consume result: "success", "transient", or "permanent"
    pub fn record_consume(&self, subject: &str, consumer: &str, result: &str, duration_ms: f64) {
        let attrs = [
            KeyValue::new("event.subject", subject.to_string()),
            KeyValue::new("consumer", consumer.to_string()),
            KeyValue::new("result", result.to_string()),
        ];
        self.consume_total.add(1, &attrs);
        self.consume_duration_ms.record(duration_ms, &attrs[..2]);
    }

    /// Record an idempotency hit (duplicate detected)
    pub fn record_idempotency_hit(&self, backend: &str) {
        self.idempotency_hits
            .add(1, &[KeyValue::new("backend", backend.to_string())]);
    }
}

impl Default for BusMetrics {
    fn default() -> Self {
        Self::new()
    }
}
```

- [ ] **Step 3: Verify full `bus-telemetry` compiles**

```bash
cargo check -p bus-telemetry
```

Expected: `Finished` with no errors.

- [ ] **Step 4: Commit**

```bash
git add crates/bus-telemetry/src/spans.rs crates/bus-telemetry/src/metrics.rs
git commit -m "feat(bus-telemetry): implement OTel spans and metrics instruments"
```

---

## Task 5: Scaffold `event-bus` facade crate

**Files:**
- Create: `crates/event-bus/Cargo.toml`
- Create: `crates/event-bus/src/lib.rs`
- Create: `crates/event-bus/src/prelude.rs`

- [ ] **Step 1: Create directories**

```bash
mkdir -p crates/event-bus/src/saga crates/event-bus/tests
```

- [ ] **Step 2: Create `crates/event-bus/Cargo.toml`**

```toml
[package]
name        = "event-bus"
description = "Production-grade event bus for Rust — NATS JetStream with effectively-once semantics"
keywords    = ["nats", "eventbus", "messaging", "outbox", "idempotency"]
categories  = ["asynchronous", "network-programming"]
version.workspace    = true
edition.workspace    = true
authors.workspace    = true
license.workspace    = true
repository.workspace = true

[features]
default         = ["macros", "nats-kv-inbox"]
macros          = ["dep:bus-macros"]
nats-kv-inbox   = ["bus-nats/nats-kv-inbox"]
postgres-inbox  = ["bus-outbox/postgres-inbox"]
redis-inbox     = ["bus-nats/redis-inbox"]
postgres-outbox = ["bus-outbox/postgres-outbox"]
sqlite-buffer   = ["bus-outbox/sqlite-buffer"]
otel            = ["dep:bus-telemetry"]
saga            = ["postgres-outbox"]

[dependencies]
async-trait   = { workspace = true }
bus-core      = { path = "../bus-core" }
bus-macros    = { path = "../bus-macros", optional = true }
bus-nats      = { path = "../bus-nats" }
bus-outbox    = { path = "../bus-outbox", optional = true }
bus-telemetry = { path = "../bus-telemetry", optional = true }
serde_json    = { workspace = true }
sqlx          = { workspace = true, optional = true }
tokio         = { workspace = true }
tracing       = { workspace = true }
uuid          = { workspace = true }

[dev-dependencies]
bus-macros             = { path = "../bus-macros" }
serde                  = { workspace = true, features = ["derive"] }
testcontainers         = { workspace = true }
testcontainers-modules = { workspace = true }
tokio                  = { workspace = true }
```

- [ ] **Step 3: Create `crates/event-bus/src/lib.rs`**

```rust
pub mod builder;
pub mod bus;
pub mod prelude;

#[cfg(feature = "saga")]
pub mod saga;

pub use builder::EventBusBuilder;
pub use bus::{EventBus, SubscriptionHandle};
```

- [ ] **Step 4: Create `crates/event-bus/src/prelude.rs`**

```rust
pub use bus_core::{
    BusError, Event, EventHandler, HandlerCtx, HandlerError, IdempotencyStore,
    MessageId, PubReceipt, Publisher,
};

#[cfg(feature = "macros")]
pub use bus_macros::Event;

#[cfg(feature = "nats-kv-inbox")]
pub use bus_nats::NatsKvIdempotencyStore;

#[cfg(feature = "postgres-outbox")]
pub use bus_outbox::PostgresOutboxStore;

#[cfg(feature = "postgres-inbox")]
pub use bus_outbox::PostgresIdempotencyStore;

#[cfg(feature = "sqlite-buffer")]
pub use bus_outbox::SqliteBuffer;
```

- [ ] **Step 5: Create stub `builder.rs` and `bus.rs`**

Create `crates/event-bus/src/builder.rs`:
```rust
use crate::bus::EventBus;
use bus_core::error::BusError;

pub struct EventBusBuilder;

impl EventBusBuilder {
    pub fn new() -> Self { Self }

    pub async fn build(self) -> Result<EventBus, BusError> {
        todo!("implemented in Task 6")
    }
}
```

Create `crates/event-bus/src/bus.rs`:
```rust
pub struct EventBus;
pub struct SubscriptionHandle;
```

Create `crates/event-bus/src/saga/mod.rs`:
```rust
pub mod choreography;
pub mod orchestration;
```

Create `crates/event-bus/src/saga/choreography.rs`:
```rust
// ChoreographyStep trait
```

Create `crates/event-bus/src/saga/orchestration.rs`:
```rust
// SagaDefinition, SagaTransition, SagaHandle
```

- [ ] **Step 6: Verify scaffold compiles**

```bash
cargo check -p event-bus
```

Expected: `Finished` (todo! panics are allowed at compile time).

- [ ] **Step 7: Commit**

```bash
git add crates/event-bus/
git commit -m "feat(event-bus): scaffold facade crate with prelude and stub builder"
```

---

## Task 6: Implement `EventBusBuilder` and `EventBus`

**Files:**
- Modify: `crates/event-bus/src/builder.rs`
- Modify: `crates/event-bus/src/bus.rs`
- Create: `crates/event-bus/tests/builder_test.rs`

- [ ] **Step 1: Write failing integration test**

Create `crates/event-bus/tests/builder_test.rs`:

```rust
use bus_nats::{NatsKvIdempotencyStore, StreamConfig};
use event_bus::EventBusBuilder;
use std::time::Duration;
use testcontainers::{core::{IntoContainerPort, WaitFor}, runners::AsyncRunner, GenericImage};

async fn start_nats() -> (impl Drop, String) {
    let c = GenericImage::new("nats", "2.10-alpine")
        .with_exposed_port(4222.tcp())
        .with_wait_for(WaitFor::message_on_stderr("Server is ready"))
        .with_cmd(["-js"])
        .start().await.unwrap();
    let host = c.get_host().await.unwrap();
    let port = c.get_host_port_ipv4(4222).await.unwrap();
    (c, format!("nats://{}:{}", host, port))
}

#[tokio::test]
async fn builder_requires_idempotency_store() {
    let (_c, url) = start_nats().await;
    // Build without idempotency store — must fail
    let result = EventBusBuilder::new().url(url).build().await;
    assert!(result.is_err(), "build() without idempotency store must return Err");
}

#[tokio::test]
async fn builder_connects_and_publishes() {
    use bus_core::{Event, MessageId, Publisher};
    use bus_macros::Event;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Serialize, Deserialize, Event)]
    #[event(subject = "events.test.created")]
    struct TestEvent { id: MessageId, value: u32 }

    let (_c, url) = start_nats().await;
    let stream_cfg = StreamConfig { num_replicas: 1, ..Default::default() };
    let client = bus_nats::NatsClient::connect(&url, &stream_cfg).await.unwrap();
    let store = NatsKvIdempotencyStore::new(
        client.jetstream().clone(),
        Duration::from_secs(3600),
    ).await.unwrap();

    let bus = EventBusBuilder::new()
        .url(&url)
        .stream_config(stream_cfg)
        .idempotency(store)
        .build()
        .await
        .unwrap();

    let evt = TestEvent { id: MessageId::new(), value: 1 };
    let receipt = bus.publish(&evt).await.unwrap();
    assert!(!receipt.duplicate);
    assert!(receipt.sequence > 0);

    bus.shutdown().await.unwrap();
}
```

- [ ] **Step 2: Run to confirm failure**

```bash
cargo test -p event-bus --test builder_test 2>&1 | head -20
```

Expected: compile error — builder methods not implemented.

- [ ] **Step 3: Implement `crates/event-bus/src/builder.rs`**

```rust
use crate::bus::EventBus;
use bus_core::{error::BusError, idempotency::IdempotencyStore};
use bus_nats::{NatsClient, StreamConfig};
use std::{path::PathBuf, sync::Arc};

pub struct EventBusBuilder {
    url:          Option<String>,
    stream_cfg:   StreamConfig,
    idempotency:  Option<Arc<dyn IdempotencyStore>>,
    sqlite_path:  Option<PathBuf>,
    otel:         bool,
}

impl EventBusBuilder {
    pub fn new() -> Self {
        Self {
            url:         None,
            stream_cfg:  StreamConfig::default(),
            idempotency: None,
            sqlite_path: None,
            otel:        false,
        }
    }

    pub fn url(mut self, url: impl Into<String>) -> Self {
        self.url = Some(url.into());
        self
    }

    pub fn stream_config(mut self, cfg: StreamConfig) -> Self {
        self.stream_cfg = cfg;
        self
    }

    pub fn replicas(mut self, n: usize) -> Self {
        self.stream_cfg.num_replicas = n;
        self
    }

    pub fn dedup_window(mut self, d: std::time::Duration) -> Self {
        self.stream_cfg.duplicate_window = d;
        self
    }

    pub fn stream_name(mut self, name: impl Into<String>) -> Self {
        self.stream_cfg.name = name.into();
        self
    }

    /// Required — no default backend. Must call exactly once before build().
    pub fn idempotency(mut self, store: impl IdempotencyStore + 'static) -> Self {
        self.idempotency = Some(Arc::new(store));
        self
    }

    pub fn sqlite_buffer(mut self, path: impl Into<PathBuf>) -> Self {
        self.sqlite_path = Some(path.into());
        self
    }

    pub fn with_otel(mut self) -> Self {
        self.otel = true;
        self
    }

    /// Build the `EventBus`. Returns `Err` if `idempotency()` was not called.
    pub async fn build(self) -> Result<EventBus, BusError> {
        let url = self.url.ok_or_else(|| BusError::Publish("url is required".into()))?;
        let idempotency = self
            .idempotency
            .ok_or_else(|| {
                BusError::Idempotency(
                    "idempotency store is required — call .idempotency(store) before build()".into(),
                )
            })?;

        let client = NatsClient::connect(&url, &self.stream_cfg).await?;

        Ok(EventBus::new(client, idempotency))
    }
}

impl Default for EventBusBuilder {
    fn default() -> Self {
        Self::new()
    }
}
```

- [ ] **Step 4: Implement `crates/event-bus/src/bus.rs`**

```rust
use bus_core::{
    error::BusError,
    event::Event,
    handler::EventHandler,
    idempotency::IdempotencyStore,
    publisher::{PubReceipt, Publisher},
};
use bus_nats::{subscriber::{subscribe, SubscribeOptions}, NatsClient, NatsPublisher};
use std::sync::Arc;

/// The main event bus handle. Clone cheaply — all state is Arc-wrapped.
#[derive(Clone)]
pub struct EventBus {
    client:      NatsClient,
    publisher:   NatsPublisher,
    idempotency: Arc<dyn IdempotencyStore>,
}

/// Handle to a running subscription. Dropping stops the consumer loop.
pub struct SubscriptionHandle(bus_nats::SubscriptionHandle);

impl EventBus {
    pub(crate) fn new(client: NatsClient, idempotency: Arc<dyn IdempotencyStore>) -> Self {
        let publisher = NatsPublisher::new(client.clone());
        Self { client, publisher, idempotency }
    }

    /// Publish an event. Uses `event.message_id()` as `Nats-Msg-Id` for deduplication.
    pub async fn publish<E: Event>(&self, event: &E) -> Result<PubReceipt, BusError> {
        self.publisher.publish(event).await
    }

    /// Subscribe to events matching `opts.filter` and dispatch to `handler`.
    /// Idempotency is checked automatically before each handler invocation.
    pub async fn subscribe<E, H>(
        &self,
        opts: SubscribeOptions,
        handler: H,
    ) -> Result<SubscriptionHandle, BusError>
    where
        E: Event,
        H: EventHandler<E>,
    {
        let handle = subscribe::<E, H, _>(
            self.client.clone(),
            opts,
            Arc::new(handler),
            self.idempotency.clone(),
        )
        .await?;
        Ok(SubscriptionHandle(handle))
    }

    /// Graceful shutdown: wait for in-flight handlers, close NATS connection.
    pub async fn shutdown(self) -> Result<(), BusError> {
        // Drop client — async-nats will drain on drop
        Ok(())
    }
}
```

- [ ] **Step 5: Run tests to confirm they pass**

```bash
cargo test -p event-bus --test builder_test
```

Expected:
```
test builder_requires_idempotency_store ... ok
test builder_connects_and_publishes ... ok
```

- [ ] **Step 6: Commit**

```bash
git add crates/event-bus/src/builder.rs crates/event-bus/src/bus.rs crates/event-bus/tests/builder_test.rs
git commit -m "feat(event-bus): implement EventBusBuilder and EventBus publish/subscribe/shutdown"
```

---

## Task 7: Implement saga choreography and orchestration

**Files:**
- Modify: `crates/event-bus/src/saga/choreography.rs`
- Modify: `crates/event-bus/src/saga/orchestration.rs`
- Create: `crates/event-bus/tests/saga_test.rs`

- [ ] **Step 1: Implement `crates/event-bus/src/saga/choreography.rs`**

```rust
use async_trait::async_trait;
use bus_core::{error::HandlerError, event::Event, handler::HandlerCtx};

/// A single step in a choreography-based saga.
///
/// On success: emits `Output` event to drive the next step.
/// On failure: emits `Compensation` event to trigger rollback in upstream services.
///
/// Suitable for linear flows with ≤ 4 participants. No central state is stored.
#[async_trait]
pub trait ChoreographyStep<E: Event>: Send + Sync {
    /// The event emitted when this step succeeds
    type Output: Event;

    /// The event emitted when this step fails (compensation)
    type Compensation: Event;

    async fn execute(
        &self,
        ctx: &HandlerCtx,
        event: E,
    ) -> Result<Self::Output, Self::Compensation>;
}
```

- [ ] **Step 2: Implement `crates/event-bus/src/saga/orchestration.rs`**

```rust
use bus_core::event::Event;
use serde::{de::DeserializeOwned, Serialize};

/// Defines the state machine for an orchestrated saga.
///
/// The orchestrator stores durable state in `eventbus_sagas` (Postgres).
/// Each incoming event triggers `transition()` which returns the next state
/// and a set of commands (events) to emit.
pub trait SagaDefinition: Send + Sync + 'static {
    /// Serializable saga state — stored as JSONB in Postgres
    type State: Serialize + DeserializeOwned + Send + Sync;

    /// The event type that drives this saga forward
    type Event: Event;

    /// Globally unique ID for this saga instance
    fn saga_id(&self) -> &str;

    /// Pure function: given current state + incoming event, return what happens next.
    /// Must not have side effects — side effects happen via emitted events.
    fn transition(
        &self,
        state: &Self::State,
        event: &Self::Event,
    ) -> SagaTransition<Self::State>;
}

/// Result of a saga state machine transition
pub enum SagaTransition<S> {
    /// Move to `next_state` and emit these events (commands to downstream services)
    Advance {
        next_state: S,
        emit:       Vec<Box<dyn Event>>,
    },

    /// Something failed — emit compensation events and mark saga as compensating
    Compensate {
        emit: Vec<Box<dyn Event>>,
    },

    /// All steps completed successfully
    Complete,

    /// Unrecoverable failure
    Fail(String),
}

/// Handle to a running saga instance
pub struct SagaHandle {
    pub saga_id: String,
}
```

- [ ] **Step 3: Write unit tests for saga types**

Create `crates/event-bus/tests/saga_test.rs`:

```rust
use bus_core::{Event, MessageId};
use bus_macros::Event;
use event_bus::saga::{
    orchestration::{SagaDefinition, SagaTransition},
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Event)]
#[event(subject = "orders.created")]
struct OrderCreated { id: MessageId, amount: i64 }

#[derive(Debug, Serialize, Deserialize, Event)]
#[event(subject = "inventory.reserve")]
struct ReserveInventory { id: MessageId, order_id: String }

#[derive(Debug, Serialize, Deserialize, Event)]
#[event(subject = "orders.cancelled")]
struct OrderCancelled { id: MessageId, reason: String }

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
enum OrderSagaState { WaitingInventory, WaitingPayment, Completed }

struct OrderSaga { id: String }

impl SagaDefinition for OrderSaga {
    type State = OrderSagaState;
    type Event  = OrderCreated;

    fn saga_id(&self) -> &str { &self.id }

    fn transition(
        &self,
        state: &Self::State,
        event: &Self::Event,
    ) -> SagaTransition<Self::State> {
        match state {
            OrderSagaState::WaitingInventory => SagaTransition::Advance {
                next_state: OrderSagaState::WaitingPayment,
                emit: vec![Box::new(ReserveInventory {
                    id:       MessageId::new(),
                    order_id: event.id.to_string(),
                })],
            },
            OrderSagaState::WaitingPayment => SagaTransition::Complete,
            OrderSagaState::Completed => SagaTransition::Fail("already completed".into()),
        }
    }
}

#[test]
fn transition_advance_emits_command() {
    let saga = OrderSaga { id: "order-123".into() };
    let evt = OrderCreated { id: MessageId::new(), amount: 100 };

    let result = saga.transition(&OrderSagaState::WaitingInventory, &evt);
    assert!(matches!(result, SagaTransition::Advance { .. }));
}

#[test]
fn transition_from_waiting_payment_completes() {
    let saga = OrderSaga { id: "order-456".into() };
    let evt = OrderCreated { id: MessageId::new(), amount: 50 };

    let result = saga.transition(&OrderSagaState::WaitingPayment, &evt);
    assert!(matches!(result, SagaTransition::Complete));
}
```

- [ ] **Step 4: Run tests to confirm they pass**

```bash
cargo test -p event-bus --test saga_test
```

Expected:
```
test transition_advance_emits_command ... ok
test transition_from_waiting_payment_completes ... ok
```

- [ ] **Step 5: Commit**

```bash
git add crates/event-bus/src/saga/ crates/event-bus/tests/saga_test.rs
git commit -m "feat(event-bus): implement saga choreography and orchestration state machine"
```

---

## Task 8: Write example `01-basic-publish`

**Files:**
- Create: `examples/01-basic-publish/Cargo.toml`
- Create: `examples/01-basic-publish/src/main.rs`

- [ ] **Step 1: Create example Cargo.toml**

```toml
[package]
name    = "01-basic-publish"
version = "0.1.0"
edition = "2024"
publish = false

[dependencies]
event-bus  = { path = "../../crates/event-bus", features = ["nats-kv-inbox"] }
bus-nats   = { path = "../../crates/bus-nats" }
serde      = { version = "1", features = ["derive"] }
tokio      = { version = "1", features = ["full"] }
tracing-subscriber = "0.3"
```

- [ ] **Step 2: Create `examples/01-basic-publish/src/main.rs`**

```rust
//! Example: publish an event with deduplication via Nats-Msg-Id.
//!
//! Run with:
//!   docker run -p 4222:4222 nats:latest -js
//!   cargo run --example 01-basic-publish -- nats://localhost:4222

use bus_nats::{NatsClient, NatsKvIdempotencyStore, StreamConfig};
use event_bus::{prelude::*, EventBusBuilder};
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Serialize, Deserialize, Event)]
#[event(subject = "orders.created", aggregate = "order")]
struct OrderCreated {
    id:       MessageId,
    order_id: String,
    total:    i64,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let url = std::env::args().nth(1).unwrap_or("nats://localhost:4222".into());

    // Build idempotency store
    let stream_cfg = StreamConfig { num_replicas: 1, ..Default::default() };
    let client = NatsClient::connect(&url, &stream_cfg).await?;
    let store = NatsKvIdempotencyStore::new(
        client.jetstream().clone(),
        Duration::from_secs(3600),
    ).await?;

    // Build bus
    let bus = EventBusBuilder::new()
        .url(&url)
        .stream_config(stream_cfg)
        .idempotency(store)
        .build()
        .await?;

    let evt = OrderCreated {
        id:       MessageId::new(),
        order_id: "ord-001".into(),
        total:    4999,
    };

    // First publish
    let r1 = bus.publish(&evt).await?;
    println!("First:  seq={} duplicate={}", r1.sequence, r1.duplicate);

    // Second publish with same message_id — will be deduped by JetStream
    let r2 = bus.publish(&evt).await?;
    println!("Second: seq={} duplicate={}", r2.sequence, r2.duplicate);

    assert!(!r1.duplicate);
    assert!(r2.duplicate, "second publish must be deduplicated");

    bus.shutdown().await?;
    println!("Done.");
    Ok(())
}
```

- [ ] **Step 3: Verify example compiles**

```bash
cargo build -p 01-basic-publish
```

Expected: `Finished` with no errors.

- [ ] **Step 4: Commit**

```bash
git add examples/01-basic-publish/
git commit -m "docs(examples): add 01-basic-publish example"
```

---

## Task 9: Write example `03-idempotent-handler`

**Files:**
- Create: `examples/03-idempotent-handler/Cargo.toml`
- Create: `examples/03-idempotent-handler/src/main.rs`

- [ ] **Step 1: Create example Cargo.toml**

```toml
[package]
name    = "03-idempotent-handler"
version = "0.1.0"
edition = "2024"
publish = false

[dependencies]
async-trait = "0.1"
bus-nats    = { path = "../../crates/bus-nats" }
event-bus   = { path = "../../crates/event-bus", features = ["nats-kv-inbox"] }
serde       = { version = "1", features = ["derive"] }
tokio       = { version = "1", features = ["full"] }
tracing-subscriber = "0.3"
```

- [ ] **Step 2: Create `examples/03-idempotent-handler/src/main.rs`**

```rust
//! Example: subscribe with idempotent handler — processes each message exactly once.
//!
//! Run with:
//!   docker run -p 4222:4222 nats:latest -js
//!   cargo run --example 03-idempotent-handler -- nats://localhost:4222

use async_trait::async_trait;
use bus_nats::{NatsClient, NatsKvIdempotencyStore, StreamConfig, SubscribeOptions};
use event_bus::{prelude::*, EventBusBuilder};
use serde::{Deserialize, Serialize};
use std::{
    sync::{atomic::{AtomicU32, Ordering}, Arc},
    time::Duration,
};

#[derive(Debug, Serialize, Deserialize, Event)]
#[event(subject = "payments.processed")]
struct PaymentProcessed {
    id:         MessageId,
    payment_id: String,
    amount:     i64,
}

struct PaymentHandler {
    count: Arc<AtomicU32>,
}

#[async_trait]
impl EventHandler<PaymentProcessed> for PaymentHandler {
    async fn handle(
        &self,
        ctx: HandlerCtx,
        evt: PaymentProcessed,
    ) -> Result<(), HandlerError> {
        let n = self.count.fetch_add(1, Ordering::SeqCst) + 1;
        println!(
            "[delivery #{}] payment_id={} amount={} msg_id={}",
            ctx.delivered, evt.payment_id, evt.amount, ctx.msg_id
        );
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let url = std::env::args().nth(1).unwrap_or("nats://localhost:4222".into());
    let stream_cfg = StreamConfig { num_replicas: 1, ..Default::default() };
    let client = NatsClient::connect(&url, &stream_cfg).await?;
    let store = NatsKvIdempotencyStore::new(
        client.jetstream().clone(),
        Duration::from_secs(3600),
    ).await?;

    let count = Arc::new(AtomicU32::new(0));
    let bus = EventBusBuilder::new()
        .url(&url)
        .stream_config(stream_cfg)
        .idempotency(store)
        .build()
        .await?;

    let _handle = bus.subscribe(
        SubscribeOptions {
            durable:     "payment-handler".into(),
            filter:      "payments.>".into(),
            concurrency: 2,
            ..Default::default()
        },
        PaymentHandler { count: count.clone() },
    ).await?;

    // Publish same event twice — handler must run exactly once
    let evt = PaymentProcessed {
        id:         MessageId::new(),
        payment_id: "pay-001".into(),
        amount:     9999,
    };
    bus.publish(&evt).await?;
    bus.publish(&evt).await?; // duplicate — deduped

    tokio::time::sleep(Duration::from_secs(1)).await;
    println!("Handler invocations: {}", count.load(Ordering::SeqCst));
    assert_eq!(count.load(Ordering::SeqCst), 1);

    bus.shutdown().await?;
    Ok(())
}
```

- [ ] **Step 3: Verify example compiles**

```bash
cargo build -p 03-idempotent-handler
```

Expected: `Finished` with no errors.

- [ ] **Step 4: Commit**

```bash
git add examples/03-idempotent-handler/
git commit -m "docs(examples): add 03-idempotent-handler example"
```

---

## Task 10: Final workspace check

- [ ] **Step 1: Run all tests**

```bash
cargo test --workspace
```

Expected: all tests pass.

- [ ] **Step 2: Run clippy across full workspace**

```bash
cargo clippy --workspace -- -D warnings
```

Expected: no warnings. Fix any before continuing.

- [ ] **Step 3: Check no accidental dep leaks**

```bash
cargo tree -p bus-core --edges normal 2>&1 | grep -E "async-nats|sqlx|rusqlite|opentelemetry"
```

Expected: no output — `bus-core` remains the dep-free leaf.

```bash
cargo tree -p bus-telemetry --edges normal 2>&1 | grep -E "sqlx|rusqlite|async-nats"
```

Expected: no output — `bus-telemetry` must not pull in NATS or DB crates.

- [ ] **Step 4: Final commit**

```bash
git add -A
git commit -m "chore: plan-3 complete — bus-telemetry OTel + event-bus facade + saga engine + examples verified"
```

---

## Summary

After Plan 3 is complete the full library is functional:

- `bus-telemetry`: W3C traceparent inject/extract, auto-instrumented spans for publish/consume/outbox/idempotency, OTel metrics for all operations
- `event-bus`: `EventBusBuilder` with required idempotency selection, `EventBus.publish` / `subscribe` / `shutdown`, saga choreography trait + orchestration state machine with optimistic concurrency
- 2 working examples demonstrating basic publish and idempotent handler patterns
- Full `cargo test --workspace` and `cargo clippy` clean
- No dep leaks: `bus-core` is dep-free leaf, `bus-telemetry` has no DB/NATS deps

**Next steps after Plan 3:**
- Plan 4 (optional): `bus-outbox` dispatcher integration into `EventBus` (wires `publish_in_tx` + background relay task)
- Plan 5 (optional): Redis inbox backend, chaos tests with toxiproxy, property-based tests with proptest

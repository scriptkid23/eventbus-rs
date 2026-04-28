# Plan 2: `bus-nats` + `bus-outbox` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the two backend crates — `bus-nats` (JetStream publisher, pull consumer, idempotency via NATS KV/Redis, DLQ, circuit breaker) and `bus-outbox` (Postgres transactional outbox, dispatcher, Postgres inbox, SQLite fallback buffer).

**Architecture:** `bus-nats` wraps `async-nats 0.46` and implements `Publisher` + consumer loop with double-ack semantics, `IdempotencyStore` via NATS KV and Redis, and a sliding-window circuit breaker that routes to SQLite when NATS is unavailable. `bus-outbox` uses `sqlx 0.8` + embedded migrations for the outbox table + inbox table, a `SELECT FOR UPDATE SKIP LOCKED` dispatcher loop, and `rusqlite` for the SQLite fallback buffer. All integration tests run against real NATS/Postgres instances via `testcontainers`.

**Tech Stack:** `async-nats 0.46`, `sqlx 0.8` (Postgres + rustls), `rusqlite 0.31`, `tokio`, `testcontainers 0.23` + `testcontainers-modules` (nats, postgres), `redis` (feature: redis-inbox)

**Prerequisite:** Plan 1 complete — `bus-core` crate available in workspace.

---

## File Map

### Workspace root
- Modify: `Cargo.toml` — add `bus-nats`, `bus-outbox` members and new workspace deps

### `crates/bus-nats/`
- Create: `crates/bus-nats/Cargo.toml`
- Create: `crates/bus-nats/src/lib.rs`
- Create: `crates/bus-nats/src/client.rs` — `NatsClient` wrapper, connect + reconnect
- Create: `crates/bus-nats/src/stream.rs` — `ensure_stream()`, default stream config
- Create: `crates/bus-nats/src/publisher.rs` — `NatsPublisher` impl of `Publisher`
- Create: `crates/bus-nats/src/ack.rs` — `double_ack()`, NAK with delay helpers
- Create: `crates/bus-nats/src/consumer.rs` — consumer config builder
- Create: `crates/bus-nats/src/subscriber.rs` — pull consumer loop, semaphore concurrency
- Create: `crates/bus-nats/src/dlq.rs` — DLQ republish on advisory
- Create: `crates/bus-nats/src/advisory.rs` — subscribe MAX_DELIVERIES, MSG_TERMINATED
- Create: `crates/bus-nats/src/circuit_breaker.rs` — sliding window circuit breaker
- Create: `crates/bus-nats/src/inbox/mod.rs`
- Create: `crates/bus-nats/src/inbox/nats_kv.rs` — `NatsKvIdempotencyStore`
- Create: `crates/bus-nats/src/inbox/redis.rs` — `RedisIdempotencyStore` (feature: redis-inbox)
- Create: `crates/bus-nats/tests/publisher_test.rs`
- Create: `crates/bus-nats/tests/subscriber_test.rs`
- Create: `crates/bus-nats/tests/circuit_breaker_test.rs`
- Create: `crates/bus-nats/tests/inbox_kv_test.rs`

### `crates/bus-outbox/`
- Create: `crates/bus-outbox/Cargo.toml`
- Create: `crates/bus-outbox/src/lib.rs`
- Create: `crates/bus-outbox/src/store.rs` — `OutboxStore` trait, `OutboxRow`
- Create: `crates/bus-outbox/src/postgres.rs` — `PostgresOutboxStore`
- Create: `crates/bus-outbox/src/dispatcher.rs` — polling dispatcher loop
- Create: `crates/bus-outbox/src/inbox_pg.rs` — `PostgresIdempotencyStore`
- Create: `crates/bus-outbox/src/sqlite.rs` — `SqliteBuffer` (feature: sqlite-buffer)
- Create: `crates/bus-outbox/migrations/001_outbox.sql`
- Create: `crates/bus-outbox/migrations/002_inbox.sql`
- Create: `crates/bus-outbox/migrations/003_sagas.sql`
- Create: `crates/bus-outbox/tests/outbox_test.rs`
- Create: `crates/bus-outbox/tests/inbox_pg_test.rs`
- Create: `crates/bus-outbox/tests/sqlite_test.rs`

### Shared test helper
- Create: `crates/bus-nats/src/testing.rs` — `start_nats_container()`, `start_postgres_container()` helpers (cfg(test) only)

---

## Task 1: Extend workspace `Cargo.toml` with new members and deps

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Add members and workspace dependencies**

Replace the `[workspace]` section in `Cargo.toml` with:

```toml
[workspace]
members = [
    "crates/bus-core",
    "crates/bus-macros",
    "crates/bus-nats",
    "crates/bus-outbox",
]
resolver = "2"

[workspace.package]
version    = "0.1.0"
edition    = "2024"
authors    = ["Olivier Taylor <tech@mey.network>"]
license    = "MIT OR Apache-2.0"
repository = "https://github.com/1hoodlabs/eventbus-rs"

[workspace.dependencies]
async-nats   = "0.46"
async-trait  = "0.1"
bytes        = "1.5"
redis        = { version = "0.26", features = ["tokio-comp", "connection-manager"] }
rusqlite     = { version = "0.31", features = ["bundled"] }
serde        = { version = "1", features = ["derive"] }
serde_json   = "1"
sqlx         = { version = "0.8", features = ["runtime-tokio", "tls-rustls-ring-webpki", "postgres", "uuid", "chrono", "json"] }
thiserror    = "1"
tokio        = { version = "1.36", features = ["full"] }
tracing      = "0.1"
uuid         = { version = "1", features = ["v7", "serde"] }
# proc-macro deps
proc-macro2  = "1"
quote        = "1"
syn          = { version = "2", features = ["full"] }
# test deps
testcontainers         = "0.23"
testcontainers-modules = { version = "0.11", features = ["nats", "postgres"] }
```

- [ ] **Step 2: Verify workspace resolves**

```bash
cargo metadata --no-deps --format-version 1 | grep '"name"' | head -10
```

Expected: lists `bus-core`, `bus-macros`, `bus-nats`, `bus-outbox`.

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml
git commit -m "chore: add bus-nats and bus-outbox to workspace, extend workspace deps"
```

---

## Task 2: Scaffold `bus-nats` crate

**Files:**
- Create: `crates/bus-nats/Cargo.toml`
- Create: `crates/bus-nats/src/lib.rs`

- [ ] **Step 1: Create directories**

```bash
mkdir -p crates/bus-nats/src/inbox crates/bus-nats/tests
```

- [ ] **Step 2: Create `crates/bus-nats/Cargo.toml`**

```toml
[package]
name        = "bus-nats"
description = "NATS JetStream backend for eventbus-rs"
keywords    = ["nats", "jetstream", "eventbus", "async"]
categories  = ["asynchronous", "network-programming"]
version.workspace    = true
edition.workspace    = true
authors.workspace    = true
license.workspace    = true
repository.workspace = true

[features]
default      = ["nats-kv-inbox"]
nats-kv-inbox = []
redis-inbox  = ["dep:redis"]

[dependencies]
async-nats  = { workspace = true }
async-trait = { workspace = true }
bus-core    = { path = "../bus-core" }
bytes       = { workspace = true }
redis       = { workspace = true, optional = true }
serde_json  = { workspace = true }
tokio       = { workspace = true }
tracing     = { workspace = true }
uuid        = { workspace = true }

[dev-dependencies]
testcontainers         = { workspace = true }
testcontainers-modules = { workspace = true }
tokio                  = { workspace = true }
serde                  = { workspace = true, features = ["derive"] }
bus-macros             = { path = "../bus-macros" }
```

- [ ] **Step 3: Create `crates/bus-nats/src/lib.rs`**

```rust
pub mod ack;
pub mod advisory;
pub mod circuit_breaker;
pub mod client;
pub mod consumer;
pub mod dlq;
pub mod inbox;
pub mod publisher;
pub mod stream;
pub mod subscriber;

#[cfg(test)]
pub(crate) mod testing;

pub use client::NatsClient;
pub use publisher::NatsPublisher;
pub use stream::StreamConfig;
pub use subscriber::{SubscribeOptions, SubscriptionHandle};

#[cfg(feature = "nats-kv-inbox")]
pub use inbox::nats_kv::NatsKvIdempotencyStore;

#[cfg(feature = "redis-inbox")]
pub use inbox::redis::RedisIdempotencyStore;
```

- [ ] **Step 4: Create stub modules** (one file each, will be filled in subsequent tasks)

Create `crates/bus-nats/src/client.rs`:
```rust
pub struct NatsClient;
```

Create `crates/bus-nats/src/stream.rs`:
```rust
pub struct StreamConfig;
```

Create `crates/bus-nats/src/publisher.rs`:
```rust
pub struct NatsPublisher;
```

Create `crates/bus-nats/src/ack.rs`:
```rust
// ACK helpers
```

Create `crates/bus-nats/src/consumer.rs`:
```rust
// Consumer config
```

Create `crates/bus-nats/src/subscriber.rs`:
```rust
pub struct SubscribeOptions;
pub struct SubscriptionHandle;
```

Create `crates/bus-nats/src/dlq.rs`:
```rust
// DLQ logic
```

Create `crates/bus-nats/src/advisory.rs`:
```rust
// Advisory subscriptions
```

Create `crates/bus-nats/src/circuit_breaker.rs`:
```rust
// Circuit breaker
```

Create `crates/bus-nats/src/inbox/mod.rs`:
```rust
#[cfg(feature = "nats-kv-inbox")]
pub mod nats_kv;
#[cfg(feature = "redis-inbox")]
pub mod redis;
```

Create `crates/bus-nats/src/inbox/nats_kv.rs`:
```rust
pub struct NatsKvIdempotencyStore;
```

Create `crates/bus-nats/src/inbox/redis.rs`:
```rust
pub struct RedisIdempotencyStore;
```

Create `crates/bus-nats/src/testing.rs`:
```rust
// Test helpers
```

- [ ] **Step 5: Verify scaffold compiles**

```bash
cargo check -p bus-nats
```

Expected: `Finished` with no errors.

- [ ] **Step 6: Commit**

```bash
git add crates/bus-nats/
git commit -m "feat(bus-nats): scaffold crate with stub modules"
```

---

## Task 3: Implement `NatsClient` — connect and stream setup

**Files:**
- Modify: `crates/bus-nats/src/client.rs`
- Modify: `crates/bus-nats/src/stream.rs`

- [ ] **Step 1: Implement `crates/bus-nats/src/stream.rs`**

```rust
use async_nats::jetstream::{self, stream};
use std::time::Duration;

/// Configuration for the JetStream stream.
/// Defaults are production-safe: R3 replication, 5-minute dedup window.
#[derive(Debug, Clone)]
pub struct StreamConfig {
    pub name:             String,
    pub subjects:         Vec<String>,
    pub num_replicas:     usize,
    pub duplicate_window: Duration,
    pub max_age:          Duration,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            name:             "EVENTS".into(),
            subjects:         vec!["events.>".into()],
            num_replicas:     3,
            duplicate_window: Duration::from_secs(5 * 60),
            max_age:          Duration::from_secs(7 * 24 * 3600),
        }
    }
}

/// Ensure the stream exists, creating it if absent.
/// Safe to call on every startup — idempotent.
pub async fn ensure_stream(
    js: &jetstream::Context,
    cfg: &StreamConfig,
) -> Result<stream::Stream, async_nats::Error> {
    js.get_or_create_stream(stream::Config {
        name:             cfg.name.clone(),
        subjects:         cfg.subjects.clone(),
        num_replicas:     cfg.num_replicas,
        storage:          stream::StorageType::File,
        duplicate_window: cfg.duplicate_window,
        discard:          stream::DiscardPolicy::Old,
        max_age:          cfg.max_age,
        allow_direct:     true,
        ..Default::default()
    })
    .await
}
```

- [ ] **Step 2: Implement `crates/bus-nats/src/client.rs`**

```rust
use crate::stream::{ensure_stream, StreamConfig};
use async_nats::jetstream;
use bus_core::error::BusError;

/// Thin wrapper over `async_nats::Client` + `jetstream::Context`.
/// Holds the stream config and ensures the stream exists on connect.
#[derive(Clone)]
pub struct NatsClient {
    pub(crate) js: jetstream::Context,
}

impl NatsClient {
    /// Connect to NATS at `url` and ensure the stream exists.
    pub async fn connect(url: &str, stream_cfg: &StreamConfig) -> Result<Self, BusError> {
        let client = async_nats::connect(url)
            .await
            .map_err(|e| BusError::Nats(e.to_string()))?;

        let js = jetstream::new(client);

        ensure_stream(&js, stream_cfg)
            .await
            .map_err(|e| BusError::Nats(e.to_string()))?;

        Ok(Self { js })
    }

    /// Return a reference to the JetStream context.
    pub fn jetstream(&self) -> &jetstream::Context {
        &self.js
    }
}
```

- [ ] **Step 3: Verify compile**

```bash
cargo check -p bus-nats
```

Expected: `Finished` with no errors.

- [ ] **Step 4: Commit**

```bash
git add crates/bus-nats/src/client.rs crates/bus-nats/src/stream.rs
git commit -m "feat(bus-nats): implement NatsClient connect and ensure_stream"
```

---

## Task 4: Implement `NatsPublisher`

**Files:**
- Modify: `crates/bus-nats/src/publisher.rs`
- Create: `crates/bus-nats/tests/publisher_test.rs`

- [ ] **Step 1: Write failing integration test**

Create `crates/bus-nats/tests/publisher_test.rs`:

```rust
use bus_core::{Event, MessageId, Publisher};
use bus_macros::Event;
use bus_nats::{NatsClient, NatsPublisher, StreamConfig};
use serde::{Deserialize, Serialize};
use testcontainers::{core::{IntoContainerPort, WaitFor}, runners::AsyncRunner, GenericImage};

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
    let r2 = publisher.publish(&evt).await.unwrap(); // same message_id

    assert!(!r1.duplicate);
    assert!(r2.duplicate, "second publish with same Nats-Msg-Id must be deduplicated");
    assert_eq!(r1.sequence, r2.sequence, "dedup must return same sequence");
}
```

- [ ] **Step 2: Run to confirm test fails**

```bash
cargo test -p bus-nats --test publisher_test 2>&1 | head -20
```

Expected: compile error — `NatsPublisher` not fully implemented.

- [ ] **Step 3: Implement `crates/bus-nats/src/publisher.rs`**

```rust
use crate::client::NatsClient;
use async_nats::HeaderMap;
use async_trait::async_trait;
use bus_core::{error::BusError, event::Event, publisher::{PubReceipt, Publisher}};
use bytes::Bytes;

/// NATS JetStream implementation of `Publisher`.
/// Attaches `Nats-Msg-Id` header for server-side deduplication.
#[derive(Clone)]
pub struct NatsPublisher {
    client: NatsClient,
}

impl NatsPublisher {
    pub fn new(client: NatsClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl Publisher for NatsPublisher {
    async fn publish<E: Event>(&self, event: &E) -> Result<PubReceipt, BusError> {
        let subject = event.subject().into_owned();
        let msg_id = event.message_id().to_string();
        let payload = serde_json::to_vec(event).map_err(BusError::Serde)?;

        let mut headers = HeaderMap::new();
        headers.insert("Nats-Msg-Id", msg_id.as_str());

        let ack_future = self
            .client
            .js
            .publish_with_headers(subject, headers, Bytes::from(payload))
            .await
            .map_err(|e| BusError::Publish(e.to_string()))?;

        let ack = ack_future
            .await
            .map_err(|e| BusError::Publish(e.to_string()))?;

        Ok(PubReceipt {
            stream:    ack.stream,
            sequence:  ack.sequence,
            duplicate: ack.duplicate,
            buffered:  false,
        })
    }
}
```

- [ ] **Step 4: Run tests to confirm they pass**

```bash
cargo test -p bus-nats --test publisher_test
```

Expected:
```
test publish_returns_receipt_with_sequence ... ok
test same_msg_id_is_deduplicated ... ok
```

- [ ] **Step 5: Commit**

```bash
git add crates/bus-nats/src/publisher.rs crates/bus-nats/tests/publisher_test.rs
git commit -m "feat(bus-nats): implement NatsPublisher with Nats-Msg-Id deduplication"
```

---

## Task 5: Implement circuit breaker

**Files:**
- Modify: `crates/bus-nats/src/circuit_breaker.rs`
- Create: `crates/bus-nats/tests/circuit_breaker_test.rs`

- [ ] **Step 1: Write failing tests**

Create `crates/bus-nats/tests/circuit_breaker_test.rs`:

```rust
use bus_nats::circuit_breaker::{CircuitBreaker, CircuitState};
use std::time::Duration;
use tokio::time::sleep;

#[tokio::test]
async fn starts_closed() {
    let cb = CircuitBreaker::new(0.5, 10, Duration::from_millis(100));
    assert_eq!(cb.state(), CircuitState::Closed);
}

#[tokio::test]
async fn opens_after_failure_threshold() {
    let cb = CircuitBreaker::new(0.5, 4, Duration::from_millis(200));
    // 3 failures out of 4 calls = 75% > 50% threshold
    cb.record_failure();
    cb.record_failure();
    cb.record_success();
    cb.record_failure();
    assert_eq!(cb.state(), CircuitState::Open);
}

#[tokio::test]
async fn transitions_to_half_open_after_timeout() {
    let cb = CircuitBreaker::new(0.5, 2, Duration::from_millis(100));
    cb.record_failure();
    cb.record_failure();
    assert_eq!(cb.state(), CircuitState::Open);

    sleep(Duration::from_millis(150)).await;
    assert_eq!(cb.state(), CircuitState::HalfOpen);
}

#[tokio::test]
async fn closes_after_half_open_success() {
    let cb = CircuitBreaker::new(0.5, 2, Duration::from_millis(100));
    cb.record_failure();
    cb.record_failure();
    sleep(Duration::from_millis(150)).await;
    assert_eq!(cb.state(), CircuitState::HalfOpen);

    cb.record_success();
    assert_eq!(cb.state(), CircuitState::Closed);
}
```

- [ ] **Step 2: Run to confirm failure**

```bash
cargo test -p bus-nats --test circuit_breaker_test 2>&1 | head -20
```

Expected: compile error — `CircuitBreaker` not implemented.

- [ ] **Step 3: Implement `crates/bus-nats/src/circuit_breaker.rs`**

```rust
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CircuitState {
    Closed,
    Open,
    HalfOpen,
}

struct Inner {
    /// Ring buffer: true=success, false=failure
    window:            VecDeque<bool>,
    window_size:       usize,
    failure_threshold: f64,
    opened_at:         Option<Instant>,
    reset_timeout:     Duration,
}

impl Inner {
    fn state(&self) -> CircuitState {
        if let Some(opened_at) = self.opened_at {
            if opened_at.elapsed() >= self.reset_timeout {
                return CircuitState::HalfOpen;
            }
            return CircuitState::Open;
        }
        CircuitState::Closed
    }

    fn failure_rate(&self) -> f64 {
        if self.window.is_empty() {
            return 0.0;
        }
        let failures = self.window.iter().filter(|&&ok| !ok).count();
        failures as f64 / self.window.len() as f64
    }

    fn record(&mut self, success: bool) {
        if self.window.len() >= self.window_size {
            self.window.pop_front();
        }
        self.window.push_back(success);

        // If currently open or half-open and a success came in, close
        if success && self.opened_at.is_some() {
            let state = self.state();
            if state == CircuitState::HalfOpen {
                self.opened_at = None;
                self.window.clear();
                return;
            }
        }

        // Check if we should open
        if self.opened_at.is_none()
            && self.window.len() >= self.window_size
            && self.failure_rate() > self.failure_threshold
        {
            self.opened_at = Some(Instant::now());
        }
    }
}

/// Sliding-window circuit breaker.
/// Opens when failure rate exceeds `failure_threshold` over last `window_size` calls.
/// Transitions to HalfOpen after `reset_timeout`; closes on next success.
#[derive(Clone)]
pub struct CircuitBreaker(Arc<Mutex<Inner>>);

impl CircuitBreaker {
    pub fn new(failure_threshold: f64, window_size: usize, reset_timeout: Duration) -> Self {
        Self(Arc::new(Mutex::new(Inner {
            window: VecDeque::with_capacity(window_size),
            window_size,
            failure_threshold,
            opened_at: None,
            reset_timeout,
        })))
    }

    pub fn state(&self) -> CircuitState {
        self.0.lock().unwrap().state()
    }

    pub fn record_success(&self) {
        self.0.lock().unwrap().record(true);
    }

    pub fn record_failure(&self) {
        self.0.lock().unwrap().record(false);
    }

    /// Returns true if requests should be allowed through (Closed or HalfOpen)
    pub fn allow_request(&self) -> bool {
        matches!(self.state(), CircuitState::Closed | CircuitState::HalfOpen)
    }
}
```

- [ ] **Step 4: Run tests to confirm they pass**

```bash
cargo test -p bus-nats --test circuit_breaker_test
```

Expected: all 4 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/bus-nats/src/circuit_breaker.rs crates/bus-nats/tests/circuit_breaker_test.rs
git commit -m "feat(bus-nats): implement sliding-window circuit breaker"
```

---

## Task 6: Implement `NatsKvIdempotencyStore`

**Files:**
- Modify: `crates/bus-nats/src/inbox/nats_kv.rs`
- Create: `crates/bus-nats/tests/inbox_kv_test.rs`

- [ ] **Step 1: Write failing integration tests**

Create `crates/bus-nats/tests/inbox_kv_test.rs`:

```rust
use bus_core::{IdempotencyStore, MessageId};
use bus_nats::{NatsClient, NatsKvIdempotencyStore, StreamConfig};
use std::time::Duration;
use testcontainers::{core::{IntoContainerPort, WaitFor}, runners::AsyncRunner, GenericImage};

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
```

- [ ] **Step 2: Run to confirm failure**

```bash
cargo test -p bus-nats --test inbox_kv_test 2>&1 | head -20
```

Expected: compile error — `NatsKvIdempotencyStore` not implemented.

- [ ] **Step 3: Implement `crates/bus-nats/src/inbox/nats_kv.rs`**

```rust
use async_nats::jetstream::{self, kv};
use async_trait::async_trait;
use bus_core::{error::BusError, id::MessageId, idempotency::IdempotencyStore};
use bytes::Bytes;
use std::time::Duration;

/// Idempotency store backed by NATS JetStream Key-Value store.
/// Uses atomic `kv.create()` (fails if key exists) to prevent duplicate processing
/// under concurrent delivery.
#[derive(Clone)]
pub struct NatsKvIdempotencyStore {
    store: kv::Store,
}

impl NatsKvIdempotencyStore {
    /// Create or open the KV bucket used for idempotency tracking.
    pub async fn new(
        js: jetstream::Context,
        max_age: Duration,
    ) -> Result<Self, BusError> {
        let store = js
            .create_key_value(kv::Config {
                bucket:      "eventbus_processed".into(),
                history:     1,
                max_age,
                num_replicas: 1,
                ..Default::default()
            })
            .await
            .map_err(|e| BusError::Idempotency(e.to_string()))?;

        Ok(Self { store })
    }
}

#[async_trait]
impl IdempotencyStore for NatsKvIdempotencyStore {
    async fn try_insert(&self, key: &MessageId, _ttl: Duration) -> Result<bool, BusError> {
        // kv.create() is atomic: fails with error if key already exists
        match self
            .store
            .create(key.to_string(), Bytes::from_static(b"pending"))
            .await
        {
            Ok(_) => Ok(true),
            Err(e) if e.to_string().contains("wrong last sequence") => Ok(false),
            Err(_) => Ok(false), // key already exists = already processed
        }
    }

    async fn mark_done(&self, key: &MessageId) -> Result<(), BusError> {
        self.store
            .put(key.to_string(), Bytes::from_static(b"done"))
            .await
            .map_err(|e| BusError::Idempotency(e.to_string()))?;
        Ok(())
    }
}
```

- [ ] **Step 4: Run tests to confirm they pass**

```bash
cargo test -p bus-nats --test inbox_kv_test
```

Expected: all 3 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/bus-nats/src/inbox/ crates/bus-nats/tests/inbox_kv_test.rs
git commit -m "feat(bus-nats): implement NatsKvIdempotencyStore with atomic KV.create"
```

---

## Task 7: Implement pull consumer subscriber loop

**Files:**
- Modify: `crates/bus-nats/src/ack.rs`
- Modify: `crates/bus-nats/src/consumer.rs`
- Modify: `crates/bus-nats/src/subscriber.rs`
- Create: `crates/bus-nats/tests/subscriber_test.rs`

- [ ] **Step 1: Implement `crates/bus-nats/src/ack.rs`**

```rust
use async_nats::jetstream::{AckKind, Message};
use bus_core::error::BusError;
use std::time::Duration;

/// Send a double-ack (waits for server confirmation).
/// This eliminates redelivery due to lost ack — required for effectively-once consumption.
pub async fn double_ack(msg: &Message) -> Result<(), BusError> {
    msg.double_ack()
        .await
        .map_err(|e| BusError::Nats(e.to_string()))
}

/// NAK with a specific delay before redelivery.
pub async fn nak_with_delay(msg: &Message, delay: Duration) -> Result<(), BusError> {
    msg.ack_with(AckKind::Nak(Some(delay)))
        .await
        .map_err(|e| BusError::Nats(e.to_string()))
}

/// Terminate message — no further redelivery, triggers MSG_TERMINATED advisory.
pub async fn term(msg: &Message) -> Result<(), BusError> {
    msg.ack_with(AckKind::Term)
        .await
        .map_err(|e| BusError::Nats(e.to_string()))
}
```

- [ ] **Step 2: Implement `crates/bus-nats/src/consumer.rs`**

```rust
use async_nats::jetstream::consumer::{pull, AckPolicy, DeliverPolicy};
use std::time::Duration;

/// Build a pull consumer config for durable, exactly-once-capable consumption.
pub fn build_pull_config(
    durable:     &str,
    filter:      &str,
    max_deliver: i64,
    ack_wait:    Duration,
    backoff:     &[Duration],
) -> pull::Config {
    pull::Config {
        durable_name:   Some(durable.into()),
        filter_subject: filter.into(),
        ack_policy:     AckPolicy::Explicit,
        deliver_policy: DeliverPolicy::All,
        ack_wait,
        max_deliver,
        backoff:        backoff.to_vec(),
        ..Default::default()
    }
}
```

- [ ] **Step 3: Implement `crates/bus-nats/src/subscriber.rs`**

```rust
use crate::{ack, client::NatsClient, consumer::build_pull_config};
use async_nats::jetstream::consumer::pull;
use async_trait::async_trait;
use bus_core::{
    error::{BusError, HandlerError},
    event::Event,
    handler::{EventHandler, HandlerCtx},
    id::MessageId,
    idempotency::IdempotencyStore,
};
use futures_util::StreamExt;
use std::{str::FromStr, sync::Arc, time::Duration};
use tokio::{sync::Semaphore, task::JoinHandle};
use tracing::Span;

/// Options for subscribing to events from a JetStream stream.
pub struct SubscribeOptions {
    pub durable:     String,
    pub filter:      String,
    pub max_deliver: i64,
    pub ack_wait:    Duration,
    pub backoff:     Vec<Duration>,
    pub concurrency: usize,
    pub dlq_subject: Option<String>,
}

impl Default for SubscribeOptions {
    fn default() -> Self {
        Self {
            durable:     "default-worker".into(),
            filter:      ">".into(),
            max_deliver: 5,
            ack_wait:    Duration::from_secs(30),
            backoff:     vec![
                Duration::from_secs(1),
                Duration::from_secs(5),
                Duration::from_secs(30),
                Duration::from_secs(300),
            ],
            concurrency: 1,
            dlq_subject: None,
        }
    }
}

/// Handle to a running subscription. Dropping this handle stops the consumer loop.
pub struct SubscriptionHandle {
    _handle: JoinHandle<()>,
}

/// Start a pull consumer loop that dispatches messages to `handler`.
/// Idempotency is checked via `idempotency_store` before handler invocation.
pub async fn subscribe<E, H, I>(
    client: NatsClient,
    opts: SubscribeOptions,
    handler: Arc<H>,
    idempotency_store: Arc<I>,
) -> Result<SubscriptionHandle, BusError>
where
    E: Event,
    H: EventHandler<E>,
    I: IdempotencyStore + 'static,
{
    let stream = client
        .js
        .get_stream(&client.js.account_info().await
            .map_err(|e| BusError::Nats(e.to_string()))
            .map(|_| "EVENTS".to_string())
            .unwrap_or("EVENTS".to_string()))
        .await
        .map_err(|e| BusError::Nats(e.to_string()))?;

    let consumer: pull::Consumer<pull::Config> = stream
        .get_or_create_consumer(
            &opts.durable,
            build_pull_config(
                &opts.durable,
                &opts.filter,
                opts.max_deliver,
                opts.ack_wait,
                &opts.backoff,
            ),
        )
        .await
        .map_err(|e| BusError::Nats(e.to_string()))?;

    let semaphore = Arc::new(Semaphore::new(opts.concurrency));
    let dlq_subject = opts.dlq_subject.clone();
    let js = client.js.clone();

    let handle = tokio::spawn(async move {
        let mut messages = match consumer.messages().await {
            Ok(m) => m,
            Err(e) => {
                tracing::error!("failed to get message stream: {}", e);
                return;
            }
        };

        while let Some(item) = messages.next().await {
            let msg = match item {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!("message stream error: {}", e);
                    continue;
                }
            };

            let permit = semaphore.clone().acquire_owned().await.unwrap();
            let handler = handler.clone();
            let store = idempotency_store.clone();
            let dlq = dlq_subject.clone();
            let js = js.clone();

            tokio::spawn(async move {
                let _permit = permit;
                process_message::<E, H, I>(msg, handler, store, dlq, js).await;
            });
        }
    });

    Ok(SubscriptionHandle { _handle: handle })
}

async fn process_message<E, H, I>(
    msg: async_nats::jetstream::Message,
    handler: Arc<H>,
    store: Arc<I>,
    dlq_subject: Option<String>,
    js: async_nats::jetstream::Context,
) where
    E: Event,
    H: EventHandler<E>,
    I: IdempotencyStore,
{
    let info = match msg.info() {
        Ok(i) => i,
        Err(e) => {
            tracing::error!("failed to get message info: {}", e);
            return;
        }
    };

    // Extract message ID from Nats-Msg-Id header, fall back to stream sequence
    let msg_id = msg
        .headers
        .as_ref()
        .and_then(|h| h.get("Nats-Msg-Id"))
        .and_then(|v| MessageId::from_str(v.as_str()).ok())
        .unwrap_or_else(|| MessageId::from_uuid(uuid::Uuid::now_v7()));

    // Idempotency check — skip if already processed
    match store.try_insert(&msg_id, Duration::from_secs(86400 * 7)).await {
        Ok(false) => {
            tracing::debug!(%msg_id, "duplicate message — skipping");
            let _ = ack::double_ack(&msg).await;
            return;
        }
        Err(e) => {
            tracing::warn!(%msg_id, "idempotency store error: {} — NAKing", e);
            let _ = ack::nak_with_delay(&msg, Duration::from_secs(1)).await;
            return;
        }
        Ok(true) => {}
    }

    // Deserialize event
    let event: E = match serde_json::from_slice(&msg.payload) {
        Ok(e) => e,
        Err(e) => {
            tracing::error!(%msg_id, "failed to deserialize event: {} — terminating", e);
            let _ = ack::term(&msg).await;
            return;
        }
    };

    let ctx = HandlerCtx {
        msg_id:     msg_id.clone(),
        stream_seq: info.stream_sequence,
        delivered:  info.delivered as u64,
        subject:    msg.subject.to_string(),
        span:       Span::current(),
    };

    match handler.handle(ctx, event).await {
        Ok(()) => {
            let _ = store.mark_done(&msg_id).await;
            let _ = ack::double_ack(&msg).await;
        }
        Err(HandlerError::Transient(reason)) => {
            tracing::warn!(%msg_id, %reason, "transient error — NAKing with backoff");
            let _ = ack::nak_with_delay(&msg, Duration::from_secs(5)).await;
        }
        Err(HandlerError::Permanent(reason)) => {
            tracing::error!(%msg_id, %reason, "permanent error — terminating");
            if let Some(dlq) = dlq_subject {
                let _ = js.publish(dlq, msg.payload.clone()).await;
            }
            let _ = ack::term(&msg).await;
        }
    }
}
```

- [ ] **Step 4: Write integration test**

Create `crates/bus-nats/tests/subscriber_test.rs`:

```rust
use bus_core::{Event, EventHandler, HandlerCtx, HandlerError, IdempotencyStore, MessageId, Publisher};
use bus_macros::Event;
use bus_nats::{NatsClient, NatsKvIdempotencyStore, NatsPublisher, StreamConfig, SubscribeOptions};
use bus_nats::subscriber::subscribe;
use serde::{Deserialize, Serialize};
use std::{sync::{Arc, atomic::{AtomicU32, Ordering}}, time::Duration};
use testcontainers::{core::{IntoContainerPort, WaitFor}, runners::AsyncRunner, GenericImage};
use async_trait::async_trait;

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

#[derive(Debug, Serialize, Deserialize, Event)]
#[event(subject = "events.test.created")]
struct TestEvent { id: MessageId, value: u32 }

struct CountingHandler(Arc<AtomicU32>);

#[async_trait]
impl EventHandler<TestEvent> for CountingHandler {
    async fn handle(&self, _ctx: HandlerCtx, _evt: TestEvent) -> Result<(), HandlerError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn duplicate_event_handled_once() {
    let (_c, url) = start_nats().await;
    let cfg = StreamConfig { num_replicas: 1, ..Default::default() };
    let client = NatsClient::connect(&url, &cfg).await.unwrap();
    let publisher = NatsPublisher::new(client.clone());
    let store = Arc::new(
        NatsKvIdempotencyStore::new(client.jetstream().clone(), Duration::from_secs(60))
            .await.unwrap()
    );

    let counter = Arc::new(AtomicU32::new(0));
    let handler = Arc::new(CountingHandler(counter.clone()));

    let opts = SubscribeOptions {
        durable: "test-worker".into(),
        filter: "events.test.>".into(),
        concurrency: 1,
        ..Default::default()
    };

    let _handle = subscribe::<TestEvent, _, _>(client, opts, handler, store).await.unwrap();

    // Publish same event twice (same message_id)
    let evt = TestEvent { id: MessageId::new(), value: 42 };
    publisher.publish(&evt).await.unwrap();
    publisher.publish(&evt).await.unwrap(); // duplicate — JetStream dedup

    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(counter.load(Ordering::SeqCst), 1, "handler must be called exactly once");
}
```

- [ ] **Step 5: Run tests**

```bash
cargo test -p bus-nats --test subscriber_test
```

Expected: `test duplicate_event_handled_once ... ok`

- [ ] **Step 6: Commit**

```bash
git add crates/bus-nats/src/ack.rs crates/bus-nats/src/consumer.rs crates/bus-nats/src/subscriber.rs crates/bus-nats/tests/subscriber_test.rs
git commit -m "feat(bus-nats): implement pull consumer loop with idempotency and double-ack"
```

---

## Task 8: Scaffold `bus-outbox` crate

**Files:**
- Create: `crates/bus-outbox/Cargo.toml`
- Create: `crates/bus-outbox/src/lib.rs`
- Create: `crates/bus-outbox/migrations/*.sql`

- [ ] **Step 1: Create directories**

```bash
mkdir -p crates/bus-outbox/src crates/bus-outbox/migrations crates/bus-outbox/tests
```

- [ ] **Step 2: Create `crates/bus-outbox/Cargo.toml`**

```toml
[package]
name        = "bus-outbox"
description = "Transactional outbox, inbox, and SQLite fallback buffer for eventbus-rs"
keywords    = ["eventbus", "outbox", "idempotency", "postgres"]
categories  = ["asynchronous", "database"]
version.workspace    = true
edition.workspace    = true
authors.workspace    = true
license.workspace    = true
repository.workspace = true

[features]
default         = ["postgres-outbox"]
postgres-outbox = ["dep:sqlx"]
postgres-inbox  = ["dep:sqlx"]
sqlite-buffer   = ["dep:rusqlite"]

[dependencies]
async-trait = { workspace = true }
bus-core    = { path = "../bus-core" }
bytes       = { workspace = true }
rusqlite    = { workspace = true, optional = true }
serde_json  = { workspace = true }
sqlx        = { workspace = true, optional = true }
tokio       = { workspace = true }
tracing     = { workspace = true }
uuid        = { workspace = true }

[dev-dependencies]
bus-macros             = { path = "../bus-macros" }
serde                  = { workspace = true, features = ["derive"] }
testcontainers         = { workspace = true }
testcontainers-modules = { workspace = true }
tokio                  = { workspace = true }
```

- [ ] **Step 3: Create SQL migrations**

Create `crates/bus-outbox/migrations/001_outbox.sql`:

```sql
CREATE TABLE IF NOT EXISTS eventbus_outbox (
    id              UUID         PRIMARY KEY,
    aggregate_type  TEXT         NOT NULL,
    aggregate_id    TEXT         NOT NULL,
    subject         TEXT         NOT NULL,
    payload         JSONB        NOT NULL,
    headers         JSONB        NOT NULL DEFAULT '{}',
    created_at      TIMESTAMPTZ  NOT NULL DEFAULT now(),
    published_at    TIMESTAMPTZ  NULL,
    attempts        INT          NOT NULL DEFAULT 0,
    last_error      TEXT         NULL
);

CREATE INDEX IF NOT EXISTS eventbus_outbox_pending_idx
    ON eventbus_outbox (created_at)
    WHERE published_at IS NULL;
```

Create `crates/bus-outbox/migrations/002_inbox.sql`:

```sql
CREATE TABLE IF NOT EXISTS eventbus_inbox (
    message_id    TEXT         NOT NULL,
    consumer      TEXT         NOT NULL,
    processed_at  TIMESTAMPTZ  NOT NULL DEFAULT now(),
    status        TEXT         NOT NULL DEFAULT 'pending',
    PRIMARY KEY (consumer, message_id)
);
```

Create `crates/bus-outbox/migrations/003_sagas.sql`:

```sql
CREATE TABLE IF NOT EXISTS eventbus_sagas (
    saga_id     TEXT         PRIMARY KEY,
    saga_type   TEXT         NOT NULL,
    state       JSONB        NOT NULL,
    status      TEXT         NOT NULL DEFAULT 'running',
    created_at  TIMESTAMPTZ  NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ  NOT NULL DEFAULT now(),
    version     INT          NOT NULL DEFAULT 0
);
```

- [ ] **Step 4: Create `crates/bus-outbox/src/lib.rs`**

```rust
#[cfg(feature = "postgres-outbox")]
pub mod dispatcher;
#[cfg(feature = "postgres-outbox")]
pub mod migrate;
#[cfg(feature = "postgres-outbox")]
pub mod postgres;
#[cfg(feature = "postgres-inbox")]
pub mod inbox_pg;
#[cfg(feature = "sqlite-buffer")]
pub mod sqlite;
pub mod store;

#[cfg(feature = "postgres-outbox")]
pub use postgres::PostgresOutboxStore;
#[cfg(feature = "postgres-inbox")]
pub use inbox_pg::PostgresIdempotencyStore;
#[cfg(feature = "sqlite-buffer")]
pub use sqlite::SqliteBuffer;
pub use store::{OutboxRow, OutboxStore};
```

- [ ] **Step 5: Verify scaffold compiles**

```bash
cargo check -p bus-outbox
```

Expected: `Finished` with no errors.

- [ ] **Step 6: Commit**

```bash
git add crates/bus-outbox/
git commit -m "feat(bus-outbox): scaffold crate with migrations"
```

---

## Task 9: Implement `OutboxStore` trait and `PostgresOutboxStore`

**Files:**
- Modify: `crates/bus-outbox/src/store.rs`
- Modify: `crates/bus-outbox/src/postgres.rs`
- Modify: `crates/bus-outbox/src/migrate.rs`
- Create: `crates/bus-outbox/tests/outbox_test.rs`

- [ ] **Step 1: Implement `crates/bus-outbox/src/store.rs`**

```rust
use async_trait::async_trait;
use bus_core::{error::BusError, event::Event, id::MessageId};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

/// A pending outbox row fetched for dispatch
#[derive(Debug, Clone)]
pub struct OutboxRow {
    pub id:             Uuid,
    pub subject:        String,
    pub payload:        serde_json::Value,
    pub headers:        serde_json::Value,
    pub attempts:       i32,
}

/// Persistent store for the transactional outbox pattern.
/// All writes happen inside the caller's DB transaction to guarantee atomicity.
#[async_trait]
pub trait OutboxStore: Send + Sync {
    /// Insert an outbox row inside an existing Postgres transaction.
    /// The event will be dispatched asynchronously by the outbox dispatcher.
    async fn insert<'tx, E: Event>(
        &self,
        tx: &mut Transaction<'tx, Postgres>,
        event: &E,
    ) -> Result<(), BusError>;

    /// Fetch up to `limit` unpublished rows, locking them with SKIP LOCKED.
    async fn fetch_pending(&self, limit: u32) -> Result<Vec<OutboxRow>, BusError>;

    /// Mark a row as successfully published.
    async fn mark_published(&self, id: &MessageId) -> Result<(), BusError>;

    /// Increment attempt count and store the error string.
    async fn mark_failed(&self, id: &MessageId, error: &str) -> Result<(), BusError>;
}
```

- [ ] **Step 2: Implement `crates/bus-outbox/src/migrate.rs`**

```rust
use sqlx::PgPool;

/// Run all embedded eventbus migrations against the given pool.
/// Safe to call on every startup — idempotent.
pub async fn run_migrations(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::migrate!("./migrations").run(pool).await
}
```

- [ ] **Step 3: Write failing integration test**

Create `crates/bus-outbox/tests/outbox_test.rs`:

```rust
use bus_core::{Event, MessageId};
use bus_macros::Event;
use bus_outbox::{migrate::run_migrations, store::OutboxStore, PostgresOutboxStore};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use testcontainers::{core::{IntoContainerPort, WaitFor}, runners::AsyncRunner, GenericImage};

async fn start_postgres() -> (impl Drop, String) {
    let c = GenericImage::new("postgres", "16-alpine")
        .with_exposed_port(5432.tcp())
        .with_wait_for(WaitFor::message_on_stderr("database system is ready to accept connections"))
        .with_env_var("POSTGRES_PASSWORD", "password")
        .with_env_var("POSTGRES_DB", "testdb")
        .start().await.unwrap();
    let host = c.get_host().await.unwrap();
    let port = c.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:password@{}:{}/testdb", host, port);
    (c, url)
}

#[derive(Debug, Serialize, Deserialize, Event)]
#[event(subject = "orders.created", aggregate = "order")]
struct OrderCreated { id: MessageId, total: i64 }

#[tokio::test]
async fn insert_and_fetch_pending() {
    let (_c, url) = start_postgres().await;
    let pool = PgPool::connect(&url).await.unwrap();
    run_migrations(&pool).await.unwrap();

    let store = PostgresOutboxStore::new(pool.clone());
    let event = OrderCreated { id: MessageId::new(), total: 100 };

    let mut tx = pool.begin().await.unwrap();
    store.insert(&mut tx, &event).await.unwrap();
    tx.commit().await.unwrap();

    let rows = store.fetch_pending(10).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].subject, "orders.created");
}

#[tokio::test]
async fn mark_published_removes_from_pending() {
    let (_c, url) = start_postgres().await;
    let pool = PgPool::connect(&url).await.unwrap();
    run_migrations(&pool).await.unwrap();

    let store = PostgresOutboxStore::new(pool.clone());
    let event = OrderCreated { id: MessageId::new(), total: 200 };

    let mut tx = pool.begin().await.unwrap();
    store.insert(&mut tx, &event).await.unwrap();
    tx.commit().await.unwrap();

    let rows = store.fetch_pending(10).await.unwrap();
    let id = MessageId::from_uuid(rows[0].id);
    store.mark_published(&id).await.unwrap();

    let rows_after = store.fetch_pending(10).await.unwrap();
    assert!(rows_after.is_empty(), "published row must not appear in pending");
}
```

- [ ] **Step 4: Run to confirm failure**

```bash
cargo test -p bus-outbox --test outbox_test 2>&1 | head -20
```

Expected: compile error — `PostgresOutboxStore` not implemented.

- [ ] **Step 5: Implement `crates/bus-outbox/src/postgres.rs`**

```rust
use crate::store::{OutboxRow, OutboxStore};
use async_trait::async_trait;
use bus_core::{error::BusError, event::Event, id::MessageId};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

/// PostgreSQL-backed transactional outbox store.
#[derive(Clone)]
pub struct PostgresOutboxStore {
    pool: PgPool,
}

impl PostgresOutboxStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl OutboxStore for PostgresOutboxStore {
    async fn insert<'tx, E: Event>(
        &self,
        tx: &mut Transaction<'tx, Postgres>,
        event: &E,
    ) -> Result<(), BusError> {
        let id = event.message_id().as_uuid().to_owned();
        let payload = serde_json::to_value(event).map_err(BusError::Serde)?;
        let aggregate_type = E::aggregate_type();
        let aggregate_id = event.message_id().to_string();
        let subject = event.subject().into_owned();

        sqlx::query!(
            r#"INSERT INTO eventbus_outbox
               (id, aggregate_type, aggregate_id, subject, payload)
               VALUES ($1, $2, $3, $4, $5)"#,
            id,
            aggregate_type,
            aggregate_id,
            subject,
            payload,
        )
        .execute(&mut **tx)
        .await
        .map_err(|e| BusError::Outbox(e.to_string()))?;

        Ok(())
    }

    async fn fetch_pending(&self, limit: u32) -> Result<Vec<OutboxRow>, BusError> {
        let rows = sqlx::query!(
            r#"SELECT id, subject, payload, headers, attempts
               FROM eventbus_outbox
               WHERE published_at IS NULL
               ORDER BY created_at
               FOR UPDATE SKIP LOCKED
               LIMIT $1"#,
            limit as i64
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| BusError::Outbox(e.to_string()))?;

        Ok(rows
            .into_iter()
            .map(|r| OutboxRow {
                id:       r.id,
                subject:  r.subject,
                payload:  r.payload,
                headers:  r.headers,
                attempts: r.attempts,
            })
            .collect())
    }

    async fn mark_published(&self, id: &MessageId) -> Result<(), BusError> {
        sqlx::query!(
            "UPDATE eventbus_outbox SET published_at = now() WHERE id = $1",
            id.as_uuid()
        )
        .execute(&self.pool)
        .await
        .map_err(|e| BusError::Outbox(e.to_string()))?;
        Ok(())
    }

    async fn mark_failed(&self, id: &MessageId, error: &str) -> Result<(), BusError> {
        sqlx::query!(
            "UPDATE eventbus_outbox SET attempts = attempts + 1, last_error = $2 WHERE id = $1",
            id.as_uuid(),
            error
        )
        .execute(&self.pool)
        .await
        .map_err(|e| BusError::Outbox(e.to_string()))?;
        Ok(())
    }
}
```

- [ ] **Step 6: Run tests to confirm they pass**

```bash
cargo test -p bus-outbox --test outbox_test
```

Expected: all 2 tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/bus-outbox/src/ crates/bus-outbox/tests/outbox_test.rs
git commit -m "feat(bus-outbox): implement PostgresOutboxStore with SKIP LOCKED dispatcher"
```

---

## Task 10: Implement `PostgresIdempotencyStore`

**Files:**
- Modify: `crates/bus-outbox/src/inbox_pg.rs`
- Create: `crates/bus-outbox/tests/inbox_pg_test.rs`

- [ ] **Step 1: Write failing tests**

Create `crates/bus-outbox/tests/inbox_pg_test.rs`:

```rust
use bus_core::{IdempotencyStore, MessageId};
use bus_outbox::{migrate::run_migrations, PostgresIdempotencyStore};
use sqlx::PgPool;
use std::time::Duration;
use testcontainers::{core::{IntoContainerPort, WaitFor}, runners::AsyncRunner, GenericImage};

async fn start_postgres() -> (impl Drop, String) {
    let c = GenericImage::new("postgres", "16-alpine")
        .with_exposed_port(5432.tcp())
        .with_wait_for(WaitFor::message_on_stderr("database system is ready to accept connections"))
        .with_env_var("POSTGRES_PASSWORD", "password")
        .with_env_var("POSTGRES_DB", "testdb")
        .start().await.unwrap();
    let host = c.get_host().await.unwrap();
    let port = c.get_host_port_ipv4(5432).await.unwrap();
    (c, format!("postgres://postgres:password@{}:{}/testdb", host, port))
}

#[tokio::test]
async fn first_insert_returns_true() {
    let (_c, url) = start_postgres().await;
    let pool = PgPool::connect(&url).await.unwrap();
    run_migrations(&pool).await.unwrap();

    let store = PostgresIdempotencyStore::new(pool, "test-consumer".into());
    let id = MessageId::new();
    assert!(store.try_insert(&id, Duration::from_secs(3600)).await.unwrap());
}

#[tokio::test]
async fn second_insert_returns_false() {
    let (_c, url) = start_postgres().await;
    let pool = PgPool::connect(&url).await.unwrap();
    run_migrations(&pool).await.unwrap();

    let store = PostgresIdempotencyStore::new(pool, "test-consumer".into());
    let id = MessageId::new();
    store.try_insert(&id, Duration::from_secs(3600)).await.unwrap();
    assert!(!store.try_insert(&id, Duration::from_secs(3600)).await.unwrap());
}

#[tokio::test]
async fn different_consumers_independent() {
    let (_c, url) = start_postgres().await;
    let pool = PgPool::connect(&url).await.unwrap();
    run_migrations(&pool).await.unwrap();

    let store_a = PostgresIdempotencyStore::new(pool.clone(), "consumer-a".into());
    let store_b = PostgresIdempotencyStore::new(pool, "consumer-b".into());
    let id = MessageId::new();

    assert!(store_a.try_insert(&id, Duration::from_secs(3600)).await.unwrap());
    // Same message_id but different consumer — must return true
    assert!(store_b.try_insert(&id, Duration::from_secs(3600)).await.unwrap());
}
```

- [ ] **Step 2: Run to confirm failure**

```bash
cargo test -p bus-outbox --test inbox_pg_test 2>&1 | head -20
```

Expected: compile error.

- [ ] **Step 3: Implement `crates/bus-outbox/src/inbox_pg.rs`**

```rust
use async_trait::async_trait;
use bus_core::{error::BusError, id::MessageId, idempotency::IdempotencyStore};
use sqlx::PgPool;
use std::time::Duration;

/// PostgreSQL-backed idempotency store.
/// Uses `INSERT ... ON CONFLICT DO NOTHING` for atomic, race-free deduplication.
/// Each consumer has an independent namespace via the `consumer` field.
#[derive(Clone)]
pub struct PostgresIdempotencyStore {
    pool:     PgPool,
    consumer: String,
}

impl PostgresIdempotencyStore {
    pub fn new(pool: PgPool, consumer: String) -> Self {
        Self { pool, consumer }
    }
}

#[async_trait]
impl IdempotencyStore for PostgresIdempotencyStore {
    async fn try_insert(&self, key: &MessageId, _ttl: Duration) -> Result<bool, BusError> {
        let result = sqlx::query!(
            r#"INSERT INTO eventbus_inbox (message_id, consumer, status)
               VALUES ($1, $2, 'pending')
               ON CONFLICT (consumer, message_id) DO NOTHING"#,
            key.to_string(),
            self.consumer,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| BusError::Idempotency(e.to_string()))?;

        // rows_affected == 1 means inserted (first time), 0 means conflict (duplicate)
        Ok(result.rows_affected() == 1)
    }

    async fn mark_done(&self, key: &MessageId) -> Result<(), BusError> {
        sqlx::query!(
            "UPDATE eventbus_inbox SET status = 'done' WHERE consumer = $1 AND message_id = $2",
            self.consumer,
            key.to_string(),
        )
        .execute(&self.pool)
        .await
        .map_err(|e| BusError::Idempotency(e.to_string()))?;
        Ok(())
    }
}
```

- [ ] **Step 4: Run tests to confirm they pass**

```bash
cargo test -p bus-outbox --test inbox_pg_test
```

Expected: all 3 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/bus-outbox/src/inbox_pg.rs crates/bus-outbox/tests/inbox_pg_test.rs
git commit -m "feat(bus-outbox): implement PostgresIdempotencyStore with ON CONFLICT DO NOTHING"
```

---

## Task 11: Implement `SqliteBuffer` fallback

**Files:**
- Modify: `crates/bus-outbox/src/sqlite.rs`
- Create: `crates/bus-outbox/tests/sqlite_test.rs`

- [ ] **Step 1: Write failing unit tests**

Create `crates/bus-outbox/tests/sqlite_test.rs`:

```rust
use bus_outbox::SqliteBuffer;
use std::time::{SystemTime, UNIX_EPOCH};

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64
}

#[test]
fn insert_and_pop_in_order() {
    let buf = SqliteBuffer::in_memory().unwrap();
    buf.insert("id-1", "events.test", b"payload1", "{}", now_ms()).unwrap();
    buf.insert("id-2", "events.test", b"payload2", "{}", now_ms() + 1).unwrap();

    let rows = buf.fetch_pending(10).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].id, "id-1");
    assert_eq!(rows[1].id, "id-2");
}

#[test]
fn delete_removes_row() {
    let buf = SqliteBuffer::in_memory().unwrap();
    buf.insert("id-1", "events.test", b"payload", "{}", now_ms()).unwrap();
    buf.delete("id-1").unwrap();

    let rows = buf.fetch_pending(10).unwrap();
    assert!(rows.is_empty());
}

#[test]
fn pending_count() {
    let buf = SqliteBuffer::in_memory().unwrap();
    assert_eq!(buf.pending_count().unwrap(), 0);
    buf.insert("id-1", "events.test", b"data", "{}", now_ms()).unwrap();
    assert_eq!(buf.pending_count().unwrap(), 1);
}
```

- [ ] **Step 2: Run to confirm failure**

```bash
cargo test -p bus-outbox --test sqlite_test --features sqlite-buffer 2>&1 | head -20
```

Expected: compile error.

- [ ] **Step 3: Implement `crates/bus-outbox/src/sqlite.rs`**

```rust
use rusqlite::{params, Connection};
use std::sync::{Arc, Mutex};

/// A row from the SQLite fallback buffer
pub struct BufferRow {
    pub id:         String,
    pub subject:    String,
    pub payload:    Vec<u8>,
    pub headers:    String,
    pub created_at: i64,
    pub attempts:   i32,
}

/// Local SQLite buffer for storing events when NATS is unavailable.
/// Thread-safe via `Arc<Mutex<Connection>>`.
#[derive(Clone)]
pub struct SqliteBuffer {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteBuffer {
    /// Open or create a SQLite database at `path`.
    pub fn open(path: &str) -> Result<Self, rusqlite::Error> {
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    /// Create an in-memory SQLite database (useful for tests).
    pub fn in_memory() -> Result<Self, rusqlite::Error> {
        let conn = Connection::open_in_memory()?;
        Self::init(conn)
    }

    fn init(conn: Connection) -> Result<Self, rusqlite::Error> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS eventbus_buffer (
                id          TEXT    PRIMARY KEY,
                subject     TEXT    NOT NULL,
                payload     BLOB    NOT NULL,
                headers     TEXT    NOT NULL DEFAULT '{}',
                created_at  INTEGER NOT NULL,
                attempts    INTEGER NOT NULL DEFAULT 0
            );",
        )?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    /// Insert an event into the buffer.
    pub fn insert(
        &self,
        id:         &str,
        subject:    &str,
        payload:    &[u8],
        headers:    &str,
        created_at: i64,
    ) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO eventbus_buffer (id, subject, payload, headers, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, subject, payload, headers, created_at],
        )?;
        Ok(())
    }

    /// Fetch up to `limit` rows ordered by `created_at` ASC (oldest first).
    pub fn fetch_pending(&self, limit: usize) -> Result<Vec<BufferRow>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, subject, payload, headers, created_at, attempts
             FROM eventbus_buffer
             ORDER BY created_at ASC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            Ok(BufferRow {
                id:         r.get(0)?,
                subject:    r.get(1)?,
                payload:    r.get(2)?,
                headers:    r.get(3)?,
                created_at: r.get(4)?,
                attempts:   r.get(5)?,
            })
        })?;
        rows.collect()
    }

    /// Delete a row by ID after successful relay.
    pub fn delete(&self, id: &str) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM eventbus_buffer WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Count pending rows (for metrics/monitoring).
    pub fn pending_count(&self) -> Result<i64, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT COUNT(*) FROM eventbus_buffer", [], |r| r.get(0))
    }
}
```

- [ ] **Step 4: Run tests to confirm they pass**

```bash
cargo test -p bus-outbox --test sqlite_test --features sqlite-buffer
```

Expected: all 3 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/bus-outbox/src/sqlite.rs crates/bus-outbox/tests/sqlite_test.rs
git commit -m "feat(bus-outbox): implement SqliteBuffer fallback with in-memory test support"
```

---

## Task 12: Final workspace check

- [ ] **Step 1: Run all tests**

```bash
cargo test --workspace
```

Expected: all tests pass.

- [ ] **Step 2: Run clippy**

```bash
cargo clippy --workspace -- -D warnings
```

Expected: no warnings. Fix any before continuing.

- [ ] **Step 3: Verify dependency isolation — bus-core still has no NATS/sqlx**

```bash
cargo tree -p bus-core --edges normal 2>&1 | grep -E "async-nats|sqlx|rusqlite"
```

Expected: no output — `bus-core` must not depend on any of these.

- [ ] **Step 4: Final commit**

```bash
git add -A
git commit -m "chore: plan-2 complete — bus-nats JetStream backend + bus-outbox Postgres/SQLite verified"
```

---

## Summary

After Plan 2 is complete you will have:

- `bus-nats`: `NatsPublisher` (dedup via `Nats-Msg-Id`), pull consumer loop (double-ack, idempotency, DLQ, backoff), `NatsKvIdempotencyStore`, sliding-window circuit breaker
- `bus-outbox`: `PostgresOutboxStore` (SKIP LOCKED dispatcher), `PostgresIdempotencyStore` (ON CONFLICT DO NOTHING), `SqliteBuffer` fallback
- All integration tests running against real NATS + Postgres via testcontainers
- `cargo test --workspace` and `cargo clippy` clean

**Plan 3** covers `bus-telemetry` (OTel spans + metrics + W3C traceparent) and the `event-bus` facade crate (builder, `EventBus`, saga engine, public API + examples).

# eventbus-rs Architecture Design

**Date:** 2026-04-28  
**Status:** Approved  
**Scope:** v1.0 — open-source crates.io library

---

## Bottom Line Up Front

`eventbus-rs` is a production-grade Rust event bus library built on NATS JetStream, providing *effectively-once* semantics via three layered guarantees: wire-level (JetStream R3 + dedup), application-level (Transactional Outbox + Inbox/Idempotency), and operational (OTel tracing, DLQ, SQLite fallback buffer). It fills a genuine gap in the Rust ecosystem — no equivalent to MassTransit/NServiceBus wrapping JetStream with idempotency and outbox exists as of April 2026.

The library deliberately avoids claiming "exactly-once" delivery (theoretically impossible per Two Generals Problem) and documents failure modes transparently.

---

## Constraints & Decisions

| Dimension | Decision | Rationale |
|---|---|---|
| Target audience | Open-source crates.io | Multiple stacks, multiple DBs |
| Idempotency default | None — user must pick backend explicitly | No hidden deps for library users |
| Outbox backend (v1.0) | PostgreSQL only | Focus; trait-based for extensibility |
| Derive macro | `#[derive(Event)]` required | DX core — manual impl too verbose |
| Saga | Choreography + orchestration state machine | Full coverage requested |
| NATS down behavior | Auto SQLite buffer + background relay | Zero-loss requirement |
| Telemetry | Full OTel + W3C traceparent propagation | Production observability |
| Code comments | English only | Open-source audience |

---

## Approach: Layered Mid-Grained Workspace (Approach C)

5 crates + 1 proc-macro crate + 1 facade. Feature flags on `event-bus` control which layers are compiled.

---

## Crate Workspace Structure

```
eventbus-rs/
├── Cargo.toml                  # [workspace] members
├── crates/
│   ├── bus-core/               # traits, types, errors — zero NATS deps
│   ├── bus-macros/             # proc-macro: #[derive(Event)]
│   ├── bus-nats/               # JetStream backend + inbox + DLQ + retry + advisory
│   ├── bus-outbox/             # transactional outbox + SQLite fallback buffer
│   ├── bus-telemetry/          # OTel + tracing + metrics
│   └── event-bus/              # public facade + builder + saga engine
├── examples/
├── tests/                      # integration tests (testcontainers)
└── benches/
```

### Dependency Graph

```
bus-macros    ──► bus-core
bus-nats      ──► bus-core
bus-outbox    ──► bus-core
bus-telemetry ──► bus-core

event-bus ──► bus-core
event-bus ──► bus-macros
event-bus ──► bus-nats
event-bus ──► bus-outbox    (feature: postgres-outbox)
event-bus ──► bus-telemetry (feature: otel)
```

`bus-core` is the leaf — no workspace dependencies.

### Feature Flags on `event-bus`

| Feature | Pulls in | Enable when |
|---|---|---|
| `macros` | `bus-macros` | `#[derive(Event)]` — **default on** |
| `nats-kv-inbox` | `bus-nats/inbox/nats_kv` | Idempotency via NATS KV Store |
| `postgres-inbox` | `bus-outbox/inbox_pg` | Idempotency via PostgreSQL |
| `redis-inbox` | `bus-nats/inbox/redis` | Idempotency via Redis |
| `postgres-outbox` | `bus-outbox` + `sqlx/postgres` | Transactional Outbox |
| `sqlite-buffer` | `bus-outbox/sqlite` | SQLite fallback when NATS down |
| `otel` | `bus-telemetry` | Full OpenTelemetry |
| `saga` | `event-bus/saga` | Orchestration state machine |

**Minimal install:**
```toml
event-bus = { version = "0.1", features = ["nats-kv-inbox"] }
```

**Production install:**
```toml
event-bus = { version = "0.1", features = ["postgres-outbox", "postgres-inbox", "sqlite-buffer", "otel"] }
```

---

## Section 1: `bus-core` — Traits & Types

Zero external dependencies (only `serde`, `thiserror`, `uuid`, `async-trait`).

### `Event` Trait

```rust
pub trait Event: Serialize + DeserializeOwned + Send + Sync + 'static {
    /// NATS subject, e.g. "orders.created"
    fn subject(&self) -> Cow<'_, str>;

    /// Unique ID used as Nats-Msg-Id and idempotency key
    fn message_id(&self) -> MessageId;

    /// Aggregate type for outbox routing
    fn aggregate_type() -> &'static str where Self: Sized { "default" }
}

/// UUIDv7 newtype — monotonic, B-tree friendly
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MessageId(Uuid);

impl MessageId {
    pub fn new() -> Self { Self(Uuid::now_v7()) }
    pub fn from_uuid(u: Uuid) -> Self { Self(u) }
    pub fn as_str(&self) -> &str { /* ... */ }
}
```

### `Publisher` Trait

```rust
#[async_trait]
pub trait Publisher: Send + Sync {
    async fn publish<E: Event>(&self, event: &E) -> Result<PubReceipt, BusError>;

    /// Default impl: sequential; backends may override with batching
    async fn publish_batch<E: Event>(&self, events: &[E]) -> Result<Vec<PubReceipt>, BusError>;
}

pub struct PubReceipt {
    pub stream:    String,
    pub sequence:  u64,
    pub duplicate: bool,    // true = JetStream dedup suppressed — do not double-count
    pub buffered:  bool,    // true = stored in SQLite buffer, not yet in NATS
}
```

### `EventHandler` Trait

```rust
#[async_trait]
pub trait EventHandler<E: Event>: Send + Sync + 'static {
    async fn handle(&self, ctx: HandlerCtx, event: E) -> Result<(), HandlerError>;
}

pub struct HandlerCtx {
    pub msg_id:     MessageId,
    pub stream_seq: u64,
    pub delivered:  u64,        // 1 = first delivery
    pub subject:    String,
    pub span:       tracing::Span,
}

#[derive(Debug, thiserror::Error)]
pub enum HandlerError {
    #[error("transient: {0}")] Transient(String),  // → NAK + retry with backoff
    #[error("permanent: {0}")] Permanent(String),  // → Term + DLQ republish
}
```

### `IdempotencyStore` Trait (no default — user must pick)

```rust
#[async_trait]
pub trait IdempotencyStore: Send + Sync {
    /// Returns Ok(true) if this is the first time seeing this key (work should run).
    /// Returns Ok(false) if already processed (skip).
    ///
    /// MUST be implemented as an atomic operation (e.g. INSERT ... ON CONFLICT DO NOTHING
    /// for Postgres, KV.Create for NATS KV, SET NX for Redis) to prevent race conditions
    /// when multiple consumer workers process the same message concurrently.
    async fn try_insert(&self, key: &MessageId, ttl: Duration) -> Result<bool, BusError>;

    async fn mark_done(&self, key: &MessageId) -> Result<(), BusError>;
}
```

**Note on `postgres-inbox` feature:** Uses a separate `eventbus_inbox` table (not the outbox table) with schema `(message_id TEXT, consumer TEXT, processed_at TIMESTAMPTZ, PRIMARY KEY (consumer, message_id))`. Resides in `bus-outbox` crate alongside outbox for shared sqlx dependency.

### `BusError`

```rust
#[derive(Debug, thiserror::Error)]
pub enum BusError {
    #[error("nats: {0}")]            Nats(String),
    #[error("publish: {0}")]         Publish(String),
    #[error("outbox: {0}")]          Outbox(String),
    #[error("idempotency: {0}")]     Idempotency(String),
    #[error("serialization: {0}")]   Serde(#[from] serde_json::Error),
    #[error("handler: {0}")]         Handler(#[from] HandlerError),
    #[error("nats unavailable")]     NatsUnavailable,
}
```

---

## Section 2: `bus-macros` — `#[derive(Event)]`

Separate `proc-macro = true` crate (required by Rust rules). Re-exported via `event-bus::prelude`.

### Usage

```rust
use event_bus::prelude::*;

#[derive(Debug, Serialize, Deserialize, Event)]
#[event(subject = "orders.{self.order_id}.created", aggregate = "order")]
pub struct OrderCreated {
    pub id:       MessageId,    // required field — compile error if missing
    pub order_id: Uuid,
    pub total:    i64,
}
```

Generated impl:

```rust
impl Event for OrderCreated {
    fn subject(&self) -> Cow<'_, str> {
        Cow::Owned(format!("orders.{}.created", self.order_id))
    }
    fn message_id(&self) -> MessageId { self.id.clone() }
    fn aggregate_type() -> &'static str { "order" }
}
```

### Compile-time Validation

- Missing `id: MessageId` field → compile error with clear message
- `{self.field}` references non-existent field → compile error
- Subject containing invalid NATS tokens → compile error

---

## Section 3: `bus-nats` — JetStream Backend

### Internal Modules

```
bus-nats/src/
├── lib.rs
├── client.rs       # NatsClient, connection management, reconnect
├── publisher.rs    # impl Publisher for NatsPublisher
├── subscriber.rs   # pull consumer loop, semaphore-based concurrency
├── stream.rs       # stream config & ensure_stream()
├── consumer.rs     # consumer config & lifecycle
├── ack.rs          # double_ack, NAK with delay, Term
├── inbox/
│   ├── nats_kv.rs  # IdempotencyStore via NATS KV (feature: nats-kv-inbox)
│   └── redis.rs    # IdempotencyStore via Redis (feature: redis-inbox)
├── dlq.rs          # DLQ republish on advisory events
└── advisory.rs     # MAX_DELIVERIES, MSG_TERMINATED advisory subscribers
```

### Default Stream Config (production-safe)

```rust
Config {
    name:             "EVENTS".into(),
    subjects:         vec!["events.>".into()],
    num_replicas:     3,                            // R3 minimum for production
    storage:          StorageType::File,
    duplicate_window: Duration::from_secs(5 * 60), // 5 min (default 2 min too short)
    discard:          DiscardPolicy::Old,
    max_age:          Duration::from_secs(7 * 24 * 3600),
    allow_direct:     true,
    ..Default::default()
}
```

### Pull Consumer Loop

```
fetch batch (max_messages, max_wait)
    │
    for each msg:
      1. extract msg_id (Nats-Msg-Id header || stream_seq fallback)
      2. idempotency_store.try_insert(msg_id)
         ├── false → already processed → double_ack, skip
         └── true  → run handler
             ├── Ok(())          → mark_done, double_ack
             ├── Err(Transient)  → NAK(backoff_delay)
             └── Err(Permanent)  → AckTerm → DLQ republish
```

Concurrency: `tokio::sync::Semaphore` with `SubscribeOptions.concurrency` permits.

### Circuit Breaker + SQLite Buffer Flow

```
publish(event)
    │
    ├── NATS reachable ──► publish_with_headers → PubAck → Ok
    │
    └── NatsUnavailable
            │
            ▼
        circuit_breaker.record_failure()
        [sliding window: 50% failure in 10 requests, reset after 5s]
            │
            ▼
        sqlite_buffer.insert(event)         (feature: sqlite-buffer)
            │
            ▼
        Ok(PubReceipt { buffered: true })

        [background relay task]
            │
        poll sqlite_buffer every 1s
        on NATS recovery: publish rows in created_at ASC order
        delete row after PubAck success
```

---

## Section 4: `bus-outbox` — Transactional Outbox + SQLite Fallback

### Internal Modules

```
bus-outbox/src/
├── lib.rs
├── store.rs        # OutboxStore trait
├── postgres.rs     # PostgresOutboxStore (feature: postgres-outbox)
├── dispatcher.rs   # polling worker: SELECT FOR UPDATE SKIP LOCKED
├── sqlite.rs       # SqliteBuffer for NATS-down fallback (feature: sqlite-buffer)
└── migrate.rs      # embedded SQL migrations via sqlx::migrate!
```

### PostgreSQL Schema

```sql
CREATE TABLE eventbus_outbox (
    id              UUID         PRIMARY KEY,    -- UUIDv7, used as Nats-Msg-Id
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

-- Partial index: only unpublished rows
CREATE INDEX eventbus_outbox_pending_idx
    ON eventbus_outbox (created_at)
    WHERE published_at IS NULL;
```

### `OutboxStore` Trait

```rust
#[async_trait]
pub trait OutboxStore: Send + Sync {
    /// Insert outbox row inside caller's transaction
    async fn insert<'tx, E: Event>(
        &self,
        tx: &mut Transaction<'tx, Postgres>,
        event: &E,
    ) -> Result<(), BusError>;

    async fn fetch_pending(&self, limit: u32) -> Result<Vec<OutboxRow>, BusError>;
    async fn mark_published(&self, id: &MessageId) -> Result<(), BusError>;
    async fn mark_failed(&self, id: &MessageId, error: &str) -> Result<(), BusError>;
}
```

### Dispatcher Loop

```
loop:
  BEGIN tx
  SELECT id, subject, payload FROM eventbus_outbox
    WHERE published_at IS NULL
    ORDER BY created_at
    FOR UPDATE SKIP LOCKED    ← safe for parallel dispatchers
    LIMIT 100

  if empty → COMMIT, sleep(250ms), continue

  for each row:
    headers["Nats-Msg-Id"] = row.id   ← dedup key (UUIDv7)
    js.publish_with_headers(row.subject, headers, row.payload)
      ├── Ok(ack) → mark_published(row.id)
      └── Err    → mark_failed(row.id, err)
  COMMIT
```

**Correctness guarantees:**
- Event exists iff business write commits (same transaction)
- `Nats-Msg-Id = outbox.id` → retry within dedup window is a no-op
- `SKIP LOCKED` enables horizontal dispatcher scaling without contention
- Beyond dedup window → consumer-side idempotency handles duplicates

### SQLite Fallback Buffer Schema

```sql
CREATE TABLE IF NOT EXISTS eventbus_buffer (
    id          TEXT    PRIMARY KEY,    -- UUIDv7 string
    subject     TEXT    NOT NULL,
    payload     BLOB    NOT NULL,
    headers     TEXT    NOT NULL DEFAULT '{}',
    created_at  INTEGER NOT NULL,       -- Unix ms
    attempts    INTEGER NOT NULL DEFAULT 0
);
```

---

## Section 5: Saga Engine — `event-bus/src/saga/`

### Choreography

No central state. Each service listens for events and emits compensating events on failure. Suitable for ≤ 4 participants with linear flows.

```rust
#[async_trait]
pub trait ChoreographyStep<E: Event>: Send + Sync {
    type Output: Event;
    type Compensation: Event;

    async fn execute(&self, ctx: &HandlerCtx, event: E)
        -> Result<Self::Output, Self::Compensation>;
}
```

### Orchestration (durable state machine)

Requires `feature = "saga"` + `feature = "postgres-outbox"`.

```rust
pub trait SagaDefinition: Send + Sync + 'static {
    type State: Serialize + DeserializeOwned + Send + Sync;
    type Event: Event;

    fn saga_id(&self) -> &str;

    fn transition(
        &self,
        state: &Self::State,
        event: &Self::Event,
    ) -> SagaTransition<Self::State>;
}

pub enum SagaTransition<S> {
    Advance { next_state: S, emit: Vec<Box<dyn Event>> },
    Compensate { emit: Vec<Box<dyn Event>> },
    Complete,
    Fail(String),
}
```

### Saga State Table

```sql
CREATE TABLE eventbus_sagas (
    saga_id     TEXT         PRIMARY KEY,
    saga_type   TEXT         NOT NULL,
    state       JSONB        NOT NULL,
    status      TEXT         NOT NULL DEFAULT 'running',
                                      -- running | completed | compensating | failed
    created_at  TIMESTAMPTZ  NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ  NOT NULL DEFAULT now(),
    version     INT          NOT NULL DEFAULT 0  -- optimistic concurrency
);
```

Transition atomicity: `UPDATE ... WHERE saga_id=$1 AND version=$2` — zero rows returned = concurrent conflict, caller retries.

### v1.0 Scope

- **In:** state machine definition, Postgres-backed durable state, choreography step trait, optimistic concurrency
- **Out:** step-level timeouts/deadlines, visual saga designer, Temporal integration (documented as external option)

---

## Section 6: `bus-telemetry` — Full OpenTelemetry

### W3C Traceparent Propagation

Publish: inject current span context into NATS message headers as `traceparent`.  
Consume: extract `traceparent` from headers, create child span — continuous trace across process boundaries.

### Auto-Instrumented Spans

| Operation | Span name | Key attributes |
|---|---|---|
| `Publisher::publish` | `eventbus.publish` | `event.subject`, `event.msg_id`, `nats.stream` |
| Consumer receive | `eventbus.receive` | `event.subject`, `nats.stream_seq`, `nats.delivered` |
| Handler execute | `eventbus.handle` | `event.msg_id`, `handler.name` |
| Outbox dispatch | `eventbus.outbox.dispatch` | `outbox.id`, `outbox.attempts` |
| Idempotency check | `eventbus.idempotency.check` | `msg_id`, `result` (new/duplicate) |

### Metrics (OpenTelemetry Metrics API)

```
eventbus.publish.total          Counter    {subject, stream, duplicate}
eventbus.publish.duration_ms    Histogram  {subject}
eventbus.consume.total          Counter    {subject, consumer, result}
eventbus.consume.duration_ms    Histogram  {subject, consumer}
eventbus.redeliveries.total     Counter    {subject, consumer}
eventbus.dlq.total              Counter    {subject, consumer}
eventbus.outbox.pending         Gauge      {aggregate_type}
eventbus.outbox.dispatch_ms     Histogram  {}
eventbus.buffer.pending         Gauge      {}
eventbus.idempotency.hits       Counter    {backend}
```

Library does not configure exporters — user wires Jaeger/OTLP/Prometheus via their OTel setup. Library picks up the global OTel provider.

---

## Section 7: `event-bus` Facade — Builder & Public API

### `EventBusBuilder`

```rust
impl EventBusBuilder {
    pub fn new() -> Self                                         // R3, 5min dedup defaults
    pub fn url(self, url: impl Into<String>) -> Self
    pub fn replicas(self, n: usize) -> Self
    pub fn dedup_window(self, d: Duration) -> Self
    pub fn stream_name(self, name: impl Into<String>) -> Self
    pub fn idempotency(self, store: impl IdempotencyStore + 'static) -> Self  // required
    pub fn outbox(self, store: impl OutboxStore + 'static) -> Self
    pub fn sqlite_buffer(self, path: impl Into<PathBuf>) -> Self
    pub fn with_otel(self) -> Self
    pub async fn build(self) -> Result<EventBus, BusError>      // errors if idempotency not set
}
```

### `EventBus`

```rust
impl EventBus {
    /// Publish — routes through outbox if configured, buffers to SQLite if NATS unavailable
    pub async fn publish<E: Event>(&self, event: &E) -> Result<PubReceipt, BusError>;

    /// Publish inside an existing DB transaction (outbox pattern)
    pub async fn publish_in_tx<'tx, E: Event>(
        &self,
        tx: &mut Transaction<'tx, Postgres>,
        event: &E,
    ) -> Result<(), BusError>;

    pub async fn subscribe<E, H>(
        &self,
        opts: SubscribeOptions,
        handler: H,
    ) -> Result<SubscriptionHandle, BusError>
    where E: Event, H: EventHandler<E>;

    pub async fn start_saga<S: SagaDefinition>(
        &self,
        saga: S,
        initial_state: S::State,
    ) -> Result<SagaHandle, BusError>;

    /// Drain in-flight messages, flush SQLite buffer, close connections
    pub async fn shutdown(self) -> Result<(), BusError>;
}
```

### `SubscribeOptions`

```rust
pub struct SubscribeOptions {
    pub durable:     String,
    pub filter:      String,                          // e.g. "orders.>"
    pub max_deliver: i64,                             // default: 5
    pub ack_wait:    Duration,                        // default: 30s
    pub backoff:     Vec<Duration>,                   // default: [1s, 5s, 30s, 5m]
    pub concurrency: usize,                           // default: 1
    pub dlq_subject: Option<String>,                  // None = drop after max_deliver
}
```

### Full Production Usage Example

```rust
let bus = EventBusBuilder::new()
    .url("nats://localhost:4222")
    .idempotency(PostgresIdempotencyStore::new(pool.clone(), Duration::from_secs(86400 * 7)))
    .outbox(PostgresOutboxStore::new(pool.clone()))
    .sqlite_buffer("/var/lib/myapp/eventbus-buffer.db")
    .with_otel()
    .build().await?;

// Atomic business write + event — no dual-write problem
let mut tx = pool.begin().await?;
sqlx::query!("INSERT INTO orders ...").execute(&mut *tx).await?;
bus.publish_in_tx(&mut tx, &OrderCreated {
    id: MessageId::new(),
    order_id,
    total,
}).await?;
tx.commit().await?;

// Subscribe with idempotency, DLQ, and backoff
bus.subscribe(
    SubscribeOptions {
        durable:  "orders-worker".into(),
        filter:   "orders.>".into(),
        backoff:  vec![
            Duration::from_secs(1),
            Duration::from_secs(5),
            Duration::from_secs(30),
            Duration::from_secs(300),
        ],
        ..Default::default()
    },
    OrderHandler,
).await?;

bus.shutdown().await?;
```

---

## End-to-End Data Flow Diagram

```
┌──────────────────┐  BEGIN tx          ┌──────────────────┐
│   Application    │──┬─ INSERT orders ─►│   PostgreSQL     │
│                  │  └─ INSERT outbox ─►│  eventbus_outbox │
│                  │    COMMIT           │  (id = UUIDv7)   │
└──────────────────┘                    └────────┬─────────┘
                                                 │ poll FOR UPDATE SKIP LOCKED
                                                 ▼
┌──────────────────┐  Nats-Msg-Id=id   ┌──────────────────┐
│ Outbox Dispatcher│──────────────────►│  NATS JetStream  │
│  (Rust task)     │◄── PubAck ────────│  R3, dedup 5min  │
└──────────────────┘                   └────────┬─────────┘
        │                                       │ pull consumer
        │ UPDATE published_at                   ▼
        ▼                             ┌──────────────────┐
    outbox row                        │  Pull Consumer   │
                                      │  AckExplicit     │
                                      │  MaxDeliver=5    │
                                      └────────┬─────────┘
                                               │
                          ┌────────────────────┼───────────────────┐
                          ▼                    ▼                   ▼
                    [Permanent err]      [Transient err]       [Success]
                    AckKind::Term        NAK(backoff)         double_ack
                          │                   │                    │
                          ▼                   ▼                    ▼
                      DLQ stream         retry with          idempotency check
                                    [1s,5s,30s,5m]          → business write
                                                            → COMMIT → ack
```

---

## Diagrams Summary

### 1. Crate dependency graph
```
bus-core ◄── bus-macros
bus-core ◄── bus-nats
bus-core ◄── bus-outbox
bus-core ◄── bus-telemetry
bus-core ◄── event-bus ──► bus-nats, bus-outbox*, bus-telemetry*
                           (* = feature-gated)
```

### 2. Consumer state machine
```
FETCH → idempotency check
           ├── duplicate → double_ack (skip)
           └── new → handler
                    ├── Ok       → mark_done → double_ack
                    ├── Transient → NAK(delay) → redeliver
                    └── Permanent → Term → DLQ
```

### 3. Circuit breaker states
```
CLOSED ──(failure rate > 50%)──► OPEN ──(reset_timeout 5s)──► HALF_OPEN
  ▲                                                                │
  └──────────────────(probe success)──────────────────────────────┘
  
OPEN: route to SQLite buffer
HALF_OPEN: send probe, drain buffer on success
```

---

## What This Library Is NOT

- Not a Kafka replacement for sustained >500K msg/s or month-long retention
- Not an exactly-once guarantee (documented as effectively-once)
- Not a Temporal replacement for complex long-running workflows
- Not a saga framework with visual designer or step-level timeouts in v1.0

---

## Key External Dependencies

| Dep | Version | Purpose |
|---|---|---|
| `async-nats` | 0.46 | Official NATS/JetStream client (Synadia-maintained) |
| `tokio` | 1.36 | Async runtime |
| `sqlx` | 0.8 | PostgreSQL outbox + inbox |
| `rusqlite` | 0.31 | SQLite fallback buffer |
| `serde` / `serde_json` | 1 | Serialization |
| `uuid` | 1 (v7) | UUIDv7 message IDs |
| `thiserror` | 1 | Error types |
| `tracing` | 0.1 | Structured logging |
| `opentelemetry` | 0.24 | OTel SDK (feature: otel) |
| `syn` / `quote` / `proc-macro2` | latest | `bus-macros` |

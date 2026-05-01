# DLQ v1 — Pattern B (Worker-Side Republish) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement production-ready Dead Letter Queue support in `eventbus-rs` using Pattern B (worker-side republish with atomic publish-then-term ordering), built into the `event-bus` facade with per-consumer DLQ streams. Pattern A (advisory-driven safety net) is deferred to v2.

**Architecture:** Source streams stay on `LimitsPolicy` (already the default). Each consumer gets a dedicated DLQ stream `DLQ_<source>_<durable>` listening on subject `dlq.<source>.<durable>`. When a worker hits a terminal failure (deserialize error, `HandlerError::Permanent`, or final-attempt `HandlerError::Transient` with `delivered >= max_deliver`), it (1) publishes the original payload + enriched headers to the DLQ subject, (2) waits for the JetStream pub-ack, and (3) only then `Term`s the original message. If the DLQ publish fails, the original is NAK'd and stays in the source stream for retry — no silent data loss. Per-consumer dedup is enforced via deterministic `Nats-Msg-Id`.

**Tech Stack:** `async-nats 0.46`, `tokio`, `testcontainers 0.23` with `nats:2.10-alpine`, existing `bus-core`/`bus-nats`/`event-bus` workspace crates.

**Prerequisite:** Plans 1–3 complete (current state of `main` branch as of 2026-05-01).

---

## File Map

### `crates/bus-nats/src/`
- **Modify:** `lib.rs` — re-export new DLQ types (`DlqConfig`, `DlqOptions`, `DlqHeaders`)
- **Replace stub:** `dlq.rs` — types, naming helpers, header enrichment, `ensure_dlq_stream`, `publish_to_dlq`
- **Modify:** `subscriber.rs` — replace `dlq_subject: Option<String>` with `dlq: Option<DlqOptions>`, fix all failure paths (deserialize, Permanent, MaxDeliver-on-Transient), use backoff array for Nak delay
- **Leave stub:** `advisory.rs` — Pattern A is v2

### `crates/event-bus/src/`
- **Modify:** `builder.rs` — add `with_dlq(DlqConfig)` method, propagate to subscribe path
- **Modify:** `bus.rs` — store optional `DlqConfig`, auto-wire `DlqOptions` when subscribe called
- **Modify:** `prelude.rs` — re-export `DlqConfig`, `DlqOptions`

### Tests
- **Create:** `crates/bus-nats/tests/dlq_test.rs` — unit + integration tests for DLQ behavior
- **Modify:** `crates/event-bus/tests/builder_test.rs` — end-to-end DLQ via builder API

---

## Design Constants (used across tasks)

All constants live at the top of `crates/bus-nats/src/dlq.rs` and are re-used by both the implementation and the tests. **Never hardcode duration arithmetic or string literals — always reference these constants.**

```rust
use std::time::Duration;

// ─── Time multipliers ──────────────────────────────────────────────────────
const SECONDS_PER_MINUTE: u64 = 60;
const SECONDS_PER_HOUR:   u64 = 60 * SECONDS_PER_MINUTE;
const SECONDS_PER_DAY:    u64 = 24 * SECONDS_PER_HOUR;

// ─── DLQ default config values ─────────────────────────────────────────────
pub const DEFAULT_DLQ_MAX_AGE_DAYS:        u64      = 30;
pub const DEFAULT_DLQ_DUPLICATE_WINDOW_MIN: u64     = 5;
pub const DEFAULT_DLQ_REPLICAS:            usize    = 3;

pub const DEFAULT_DLQ_MAX_AGE: Duration =
    Duration::from_secs(DEFAULT_DLQ_MAX_AGE_DAYS * SECONDS_PER_DAY);

pub const DEFAULT_DLQ_DUPLICATE_WINDOW: Duration =
    Duration::from_secs(DEFAULT_DLQ_DUPLICATE_WINDOW_MIN * SECONDS_PER_MINUTE);

// Fallback delay for Transient NAK when backoff array is empty
pub const FALLBACK_NAK_DELAY: Duration = Duration::from_secs(5);

// Delay for NAK when DLQ publish fails (retry source-side soon)
pub const DLQ_FAILURE_NAK_DELAY: Duration = Duration::from_secs(5);

// ─── Naming helpers ────────────────────────────────────────────────────────
fn dlq_stream_name(source: &str, durable: &str) -> String {
    format!("DLQ_{}_{}", source, durable)
}

fn dlq_subject(source: &str, durable: &str) -> String {
    format!("dlq.{}.{}", source, durable)
}

// ─── Failure class strings (used in X-Failure-Class header) ───────────────
pub const CLASS_PERMANENT:           &str = "permanent";
pub const CLASS_POISON:              &str = "poison";
pub const CLASS_TRANSIENT_EXHAUSTED: &str = "transient_exhausted";

// ─── Failure reason strings (used in X-Failure-Reason header) ─────────────
pub const REASON_INVALID_PAYLOAD:      &str = "invalid_payload";
pub const REASON_HANDLER_PERMANENT:    &str = "handler_permanent";
pub const REASON_MAX_RETRIES_EXCEEDED: &str = "max_retries_exceeded";

// ─── Header name constants ─────────────────────────────────────────────────
pub const HDR_NATS_MSG_ID:       &str = "Nats-Msg-Id";
pub const HDR_ORIGINAL_SUBJECT:  &str = "X-Original-Subject";
pub const HDR_ORIGINAL_STREAM:   &str = "X-Original-Stream";
pub const HDR_ORIGINAL_SEQ:      &str = "X-Original-Seq";
pub const HDR_ORIGINAL_MSG_ID:   &str = "X-Original-Msg-Id";
pub const HDR_FAILURE_REASON:    &str = "X-Failure-Reason";
pub const HDR_FAILURE_CLASS:     &str = "X-Failure-Class";
pub const HDR_FAILURE_DETAIL:    &str = "X-Failure-Detail";
pub const HDR_RETRY_COUNT:       &str = "X-Retry-Count";
pub const HDR_CONSUMER:          &str = "X-Consumer";
pub const HDR_FAILED_AT:         &str = "X-Failed-At";
```

---

## Task 1: Add DLQ types and naming helpers (`dlq.rs` foundation)

**Files:**
- Replace: `crates/bus-nats/src/dlq.rs`
- Test: `crates/bus-nats/tests/dlq_test.rs`

- [ ] **Step 1: Write failing unit tests**

Create `crates/bus-nats/tests/dlq_test.rs`:

```rust
use bus_nats::dlq::{
    dlq_stream_name, dlq_subject, DlqConfig, DlqOptions,
    DEFAULT_DLQ_DUPLICATE_WINDOW, DEFAULT_DLQ_MAX_AGE, DEFAULT_DLQ_REPLICAS,
};

#[test]
fn dlq_stream_name_uses_source_and_durable() {
    assert_eq!(dlq_stream_name("EVENTS", "billing"), "DLQ_EVENTS_billing");
    assert_eq!(dlq_stream_name("ORDERS", "fulfillment-worker"), "DLQ_ORDERS_fulfillment-worker");
}

#[test]
fn dlq_subject_uses_lowercase_dot_pattern() {
    assert_eq!(dlq_subject("EVENTS", "billing"), "dlq.EVENTS.billing");
}

#[test]
fn dlq_config_defaults_are_production_safe() {
    let cfg = DlqConfig::default();
    assert_eq!(cfg.num_replicas, DEFAULT_DLQ_REPLICAS);
    assert_eq!(cfg.max_age, DEFAULT_DLQ_MAX_AGE);
    assert_eq!(cfg.duplicate_window, DEFAULT_DLQ_DUPLICATE_WINDOW);
    assert!(cfg.deny_delete);
    assert!(!cfg.deny_purge);
    assert!(cfg.allow_direct);
}

#[test]
fn dlq_options_inherits_config_defaults() {
    let opts = DlqOptions::default();
    assert_eq!(opts.config.num_replicas, DEFAULT_DLQ_REPLICAS);
}
```

- [ ] **Step 2: Run tests to verify they fail**

```
cd crates/bus-nats && cargo test --test dlq_test
```

Expected: compilation errors — `dlq` module items don't exist yet.

- [ ] **Step 3: Implement types and helpers**

Replace `crates/bus-nats/src/dlq.rs` with the following. Note the constants block at the top — these are the **single source of truth** for all duration/string values in the DLQ module. Tests, defaults, and call sites all reference these names; no inline arithmetic or duplicated string literals are allowed.

```rust
//! Dead Letter Queue (DLQ) — Pattern B (worker-side republish).
//!
//! When a worker encounters a terminal failure, it publishes the original
//! payload (plus enriched headers) into a per-consumer DLQ stream, then
//! Terms the original message. The DLQ stream uses `LimitsPolicy` retention
//! with `DenyDelete` for audit integrity.

use std::time::Duration;

// ─── Time multipliers ──────────────────────────────────────────────────────
const SECONDS_PER_MINUTE: u64 = 60;
const SECONDS_PER_HOUR:   u64 = 60 * SECONDS_PER_MINUTE;
const SECONDS_PER_DAY:    u64 = 24 * SECONDS_PER_HOUR;

// ─── DLQ default config values ─────────────────────────────────────────────
pub const DEFAULT_DLQ_MAX_AGE_DAYS:         u64   = 30;
pub const DEFAULT_DLQ_DUPLICATE_WINDOW_MIN: u64   = 5;
pub const DEFAULT_DLQ_REPLICAS:             usize = 3;

pub const DEFAULT_DLQ_MAX_AGE: Duration =
    Duration::from_secs(DEFAULT_DLQ_MAX_AGE_DAYS * SECONDS_PER_DAY);

pub const DEFAULT_DLQ_DUPLICATE_WINDOW: Duration =
    Duration::from_secs(DEFAULT_DLQ_DUPLICATE_WINDOW_MIN * SECONDS_PER_MINUTE);

pub const FALLBACK_NAK_DELAY:    Duration = Duration::from_secs(5);
pub const DLQ_FAILURE_NAK_DELAY: Duration = Duration::from_secs(5);

// ─── Failure class strings (X-Failure-Class header values) ────────────────
pub const CLASS_PERMANENT:           &str = "permanent";
pub const CLASS_POISON:              &str = "poison";
pub const CLASS_TRANSIENT_EXHAUSTED: &str = "transient_exhausted";

// ─── Failure reason strings (X-Failure-Reason header values) ──────────────
pub const REASON_INVALID_PAYLOAD:      &str = "invalid_payload";
pub const REASON_HANDLER_PERMANENT:    &str = "handler_permanent";
pub const REASON_MAX_RETRIES_EXCEEDED: &str = "max_retries_exceeded";

// ─── Header name constants ─────────────────────────────────────────────────
pub const HDR_NATS_MSG_ID:      &str = "Nats-Msg-Id";
pub const HDR_ORIGINAL_SUBJECT: &str = "X-Original-Subject";
pub const HDR_ORIGINAL_STREAM:  &str = "X-Original-Stream";
pub const HDR_ORIGINAL_SEQ:     &str = "X-Original-Seq";
pub const HDR_ORIGINAL_MSG_ID:  &str = "X-Original-Msg-Id";
pub const HDR_FAILURE_REASON:   &str = "X-Failure-Reason";
pub const HDR_FAILURE_CLASS:    &str = "X-Failure-Class";
pub const HDR_FAILURE_DETAIL:   &str = "X-Failure-Detail";
pub const HDR_RETRY_COUNT:      &str = "X-Retry-Count";
pub const HDR_CONSUMER:         &str = "X-Consumer";
pub const HDR_FAILED_AT:        &str = "X-Failed-At";

/// Configuration for DLQ streams. One DLQ stream is created per consumer.
#[derive(Debug, Clone)]
pub struct DlqConfig {
    /// Replication factor. Default: [`DEFAULT_DLQ_REPLICAS`] (matches source SLA).
    pub num_replicas: usize,
    /// Retention age for DLQ entries. Default: [`DEFAULT_DLQ_MAX_AGE`] (≥ source × 2).
    pub max_age: Duration,
    /// Maximum DLQ stream size in bytes. `None` = no cap.
    pub max_bytes: Option<i64>,
    /// JetStream dedup window for `Nats-Msg-Id`. Default: [`DEFAULT_DLQ_DUPLICATE_WINDOW`].
    pub duplicate_window: Duration,
    /// Reject `STREAM.DELETE.MSG` requests (audit integrity). Default: true.
    pub deny_delete: bool,
    /// Reject `STREAM.PURGE` requests. Default: false (allow drain after replay).
    pub deny_purge: bool,
    /// Enable direct-get for fast operator lookup. Default: true.
    pub allow_direct: bool,
}

impl Default for DlqConfig {
    fn default() -> Self {
        Self {
            num_replicas:     DEFAULT_DLQ_REPLICAS,
            max_age:          DEFAULT_DLQ_MAX_AGE,
            max_bytes:        None,
            duplicate_window: DEFAULT_DLQ_DUPLICATE_WINDOW,
            deny_delete:      true,
            deny_purge:       false,
            allow_direct:     true,
        }
    }
}

/// Per-subscribe DLQ options. Inherits from `DlqConfig` but allows override.
#[derive(Debug, Clone, Default)]
pub struct DlqOptions {
    pub config: DlqConfig,
}

/// Build the DLQ stream name for a given source stream + consumer durable.
/// Format: `DLQ_<source>_<durable>` (e.g. `DLQ_EVENTS_billing`).
pub fn dlq_stream_name(source_stream: &str, durable: &str) -> String {
    format!("DLQ_{}_{}", source_stream, durable)
}

/// Build the DLQ subject for a given source stream + consumer durable.
/// Format: `dlq.<source>.<durable>` (e.g. `dlq.EVENTS.billing`).
pub fn dlq_subject(source_stream: &str, durable: &str) -> String {
    format!("dlq.{}.{}", source_stream, durable)
}
```

Add module re-exports in `crates/bus-nats/src/lib.rs`:

```rust
pub use dlq::{
    DlqConfig, DlqOptions,
    DEFAULT_DLQ_DUPLICATE_WINDOW, DEFAULT_DLQ_MAX_AGE, DEFAULT_DLQ_REPLICAS,
};
```

- [ ] **Step 4: Run tests to verify they pass**

```
cd crates/bus-nats && cargo test --test dlq_test
```

Expected: 4 passed.

- [ ] **Step 5: Commit**

```
git add crates/bus-nats/src/dlq.rs crates/bus-nats/src/lib.rs crates/bus-nats/tests/dlq_test.rs
git commit -m "feat(bus-nats): add DlqConfig, DlqOptions, naming helpers"
```

---

## Task 2: Implement `ensure_dlq_stream`

**Files:**
- Modify: `crates/bus-nats/src/dlq.rs`
- Test: `crates/bus-nats/tests/dlq_test.rs`

- [ ] **Step 1: Write failing integration test**

Append to `crates/bus-nats/tests/dlq_test.rs`:

```rust
use async_nats::jetstream;
use bus_nats::dlq::{ensure_dlq_stream, DlqConfig};
use testcontainers::{
    GenericImage, ImageExt,
    core::{IntoContainerPort, WaitFor},
    runners::AsyncRunner,
};

async fn start_nats() -> (impl Drop, String) {
    let c = GenericImage::new("nats", "2.10-alpine")
        .with_exposed_port(4222.tcp())
        .with_wait_for(WaitFor::message_on_stderr("Server is ready"))
        .with_cmd(["-js"])
        .start()
        .await
        .unwrap();
    let host = c.get_host().await.unwrap();
    let port = c.get_host_port_ipv4(4222).await.unwrap();
    (c, format!("nats://{}:{}", host, port))
}

#[tokio::test]
async fn ensure_dlq_stream_creates_stream_with_correct_config() {
    let (_c, url) = start_nats().await;
    let client = async_nats::connect(&url).await.unwrap();
    let js = jetstream::new(client);

    let cfg = DlqConfig {
        num_replicas: 1, // single-node test
        ..Default::default()
    };
    let stream_name = "DLQ_TEST_worker";
    let subject = "dlq.TEST.worker";

    ensure_dlq_stream(&js, stream_name, subject, &cfg).await.unwrap();

    let info = js.get_stream(stream_name).await.unwrap().get_info().await.unwrap();
    assert_eq!(info.config.subjects, vec![subject.to_string()]);
    assert_eq!(info.config.num_replicas, 1);
    assert_eq!(info.config.max_age, cfg.max_age);
    assert!(info.config.deny_delete);
    assert!(info.config.allow_direct);
}

#[tokio::test]
async fn ensure_dlq_stream_is_idempotent() {
    let (_c, url) = start_nats().await;
    let client = async_nats::connect(&url).await.unwrap();
    let js = jetstream::new(client);
    let cfg = DlqConfig { num_replicas: 1, ..Default::default() };

    ensure_dlq_stream(&js, "DLQ_X_y", "dlq.X.y", &cfg).await.unwrap();
    // Second call must not error
    ensure_dlq_stream(&js, "DLQ_X_y", "dlq.X.y", &cfg).await.unwrap();
}
```

- [ ] **Step 2: Run tests to verify they fail**

```
cd crates/bus-nats && cargo test --test dlq_test ensure_dlq_stream
```

Expected: compile error — `ensure_dlq_stream` not found.

- [ ] **Step 3: Implement `ensure_dlq_stream`**

Append to `crates/bus-nats/src/dlq.rs`:

```rust
use async_nats::jetstream::{self, stream};
use bus_core::error::BusError;

/// Ensure the DLQ stream exists, creating it if absent. Idempotent.
///
/// The stream is configured with `LimitsPolicy` retention, file storage,
/// and the audit-integrity flags from `DlqConfig`.
pub async fn ensure_dlq_stream(
    js: &jetstream::Context,
    stream_name: &str,
    subject: &str,
    cfg: &DlqConfig,
) -> Result<stream::Stream, BusError> {
    let mut stream_cfg = stream::Config {
        name:             stream_name.to_string(),
        subjects:         vec![subject.to_string()],
        num_replicas:     cfg.num_replicas,
        storage:          stream::StorageType::File,
        retention:        stream::RetentionPolicy::Limits,
        discard:          stream::DiscardPolicy::Old,
        max_age:          cfg.max_age,
        duplicate_window: cfg.duplicate_window,
        deny_delete:      cfg.deny_delete,
        deny_purge:       cfg.deny_purge,
        allow_direct:     cfg.allow_direct,
        ..Default::default()
    };
    if let Some(max_bytes) = cfg.max_bytes {
        stream_cfg.max_bytes = max_bytes;
    }

    js.get_or_create_stream(stream_cfg)
        .await
        .map_err(|e| BusError::Nats(format!("ensure dlq stream {stream_name}: {e}")))
}
```

- [ ] **Step 4: Run tests to verify they pass**

```
cd crates/bus-nats && cargo test --test dlq_test ensure_dlq_stream
```

Expected: 2 passed.

- [ ] **Step 5: Commit**

```
git add crates/bus-nats/src/dlq.rs crates/bus-nats/tests/dlq_test.rs
git commit -m "feat(bus-nats): add ensure_dlq_stream for per-consumer DLQ streams"
```

---

## Task 3: Implement header enrichment (`build_dlq_headers`)

**Files:**
- Modify: `crates/bus-nats/src/dlq.rs`
- Test: `crates/bus-nats/tests/dlq_test.rs`

- [ ] **Step 1: Write failing unit tests**

Append to `crates/bus-nats/tests/dlq_test.rs`:

```rust
use bus_nats::dlq::{build_dlq_headers, FailureInfo};

#[test]
fn build_dlq_headers_includes_required_fields() {
    let info = FailureInfo {
        original_subject: "events.order.created".into(),
        original_stream:  "EVENTS".into(),
        original_seq:     42,
        original_msg_id:  "01HW...".into(),
        consumer:         "billing".into(),
        delivered:        5,
        failure_reason:   "handler_permanent".into(),
        failure_class:    "permanent".into(),
        failure_detail:   "invalid card number".into(),
    };

    let h = build_dlq_headers(&info);

    assert_eq!(h.get("X-Original-Subject").unwrap().as_str(), "events.order.created");
    assert_eq!(h.get("X-Original-Stream").unwrap().as_str(), "EVENTS");
    assert_eq!(h.get("X-Original-Seq").unwrap().as_str(), "42");
    assert_eq!(h.get("X-Original-Msg-Id").unwrap().as_str(), "01HW...");
    assert_eq!(h.get("X-Consumer").unwrap().as_str(), "billing");
    assert_eq!(h.get("X-Retry-Count").unwrap().as_str(), "5");
    assert_eq!(h.get("X-Failure-Reason").unwrap().as_str(), "handler_permanent");
    assert_eq!(h.get("X-Failure-Class").unwrap().as_str(), "permanent");
    assert_eq!(h.get("X-Failure-Detail").unwrap().as_str(), "invalid card number");
    assert!(h.get("X-Failed-At").is_some(), "X-Failed-At must be set");
}

#[test]
fn build_dlq_headers_sets_deterministic_dedup_id() {
    let info = FailureInfo {
        original_subject: "events.x".into(),
        original_stream:  "EVENTS".into(),
        original_seq:     7,
        original_msg_id:  "abc".into(),
        consumer:         "w".into(),
        delivered:        3,
        failure_reason:   "permanent".into(),
        failure_class:    "permanent".into(),
        failure_detail:   "".into(),
    };

    let h1 = build_dlq_headers(&info);
    let h2 = build_dlq_headers(&info);

    let id1 = h1.get("Nats-Msg-Id").unwrap().as_str();
    let id2 = h2.get("Nats-Msg-Id").unwrap().as_str();
    assert_eq!(id1, id2, "Nats-Msg-Id must be deterministic for same FailureInfo");
    assert_eq!(id1, "EVENTS:7:w:3");
}
```

- [ ] **Step 2: Run tests to verify they fail**

```
cd crates/bus-nats && cargo test --test dlq_test build_dlq_headers
```

Expected: compile error — `build_dlq_headers` and `FailureInfo` not found.

- [ ] **Step 3: Implement headers**

Append to `crates/bus-nats/src/dlq.rs`:

```rust
use async_nats::HeaderMap;

/// Failure metadata captured by the worker, used to enrich DLQ headers.
#[derive(Debug, Clone)]
pub struct FailureInfo {
    pub original_subject: String,
    pub original_stream:  String,
    pub original_seq:     u64,
    pub original_msg_id:  String,
    pub consumer:         String,
    pub delivered:        u64,
    pub failure_reason:   String,
    pub failure_class:    String,
    pub failure_detail:   String,
}

/// Build the DLQ HeaderMap with enrichment + deterministic `Nats-Msg-Id`.
///
/// `Nats-Msg-Id` format: `<original_stream>:<original_seq>:<consumer>:<delivered>`.
/// This ensures dedup if the worker crashes after publish but before Term —
/// on retry, the same headers produce the same `Nats-Msg-Id` and JetStream
/// dedup window drops the duplicate.
pub fn build_dlq_headers(info: &FailureInfo) -> HeaderMap {
    let mut h = HeaderMap::new();

    let dedup_id = format!(
        "{}:{}:{}:{}",
        info.original_stream, info.original_seq, info.consumer, info.delivered
    );
    h.insert(HDR_NATS_MSG_ID, dedup_id.as_str());

    h.insert(HDR_ORIGINAL_SUBJECT, info.original_subject.as_str());
    h.insert(HDR_ORIGINAL_STREAM,  info.original_stream.as_str());
    h.insert(HDR_ORIGINAL_SEQ,     info.original_seq.to_string().as_str());
    h.insert(HDR_ORIGINAL_MSG_ID,  info.original_msg_id.as_str());
    h.insert(HDR_CONSUMER,         info.consumer.as_str());
    h.insert(HDR_RETRY_COUNT,      info.delivered.to_string().as_str());
    h.insert(HDR_FAILURE_REASON,   info.failure_reason.as_str());
    h.insert(HDR_FAILURE_CLASS,    info.failure_class.as_str());
    h.insert(HDR_FAILURE_DETAIL,   info.failure_detail.as_str());

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|_| "0".into());
    h.insert(HDR_FAILED_AT, now.as_str());

    h
}
```

Add `pub use dlq::{FailureInfo, build_dlq_headers};` to `crates/bus-nats/src/lib.rs`.

- [ ] **Step 4: Run tests to verify they pass**

```
cd crates/bus-nats && cargo test --test dlq_test build_dlq_headers
```

Expected: 2 passed.

- [ ] **Step 5: Commit**

```
git add crates/bus-nats/src/dlq.rs crates/bus-nats/src/lib.rs crates/bus-nats/tests/dlq_test.rs
git commit -m "feat(bus-nats): enrich DLQ entries with X-* metadata headers + deterministic dedup id"
```

---

## Task 4: Implement atomic `publish_to_dlq` (waits for pub-ack)

**Files:**
- Modify: `crates/bus-nats/src/dlq.rs`
- Test: `crates/bus-nats/tests/dlq_test.rs`

- [ ] **Step 1: Write failing integration tests**

Append to `crates/bus-nats/tests/dlq_test.rs`:

```rust
use bus_nats::dlq::publish_to_dlq;
use bytes::Bytes;
use futures_util::StreamExt;

#[tokio::test]
async fn publish_to_dlq_persists_payload_and_headers() {
    let (_c, url) = start_nats().await;
    let client = async_nats::connect(&url).await.unwrap();
    let js = jetstream::new(client);
    let cfg = DlqConfig { num_replicas: 1, ..Default::default() };

    ensure_dlq_stream(&js, "DLQ_X_w", "dlq.X.w", &cfg).await.unwrap();

    let info = FailureInfo {
        original_subject: "events.x".into(),
        original_stream:  "X".into(),
        original_seq:     1,
        original_msg_id:  "m1".into(),
        consumer:         "w".into(),
        delivered:        5,
        failure_reason:   "permanent".into(),
        failure_class:    "permanent".into(),
        failure_detail:   "boom".into(),
    };
    let headers = build_dlq_headers(&info);
    let payload = Bytes::from_static(b"{\"hello\":\"world\"}");

    publish_to_dlq(&js, "dlq.X.w", payload.clone(), headers).await.unwrap();

    // Read it back via direct-get
    let stream = js.get_stream("DLQ_X_w").await.unwrap();
    let raw = stream.direct_get_first_for_subject("dlq.X.w").await.unwrap();
    assert_eq!(raw.payload, payload);
    assert_eq!(raw.headers.unwrap().get("X-Failure-Reason").unwrap().as_str(), "permanent");
}

#[tokio::test]
async fn publish_to_dlq_returns_err_when_no_stream_captures_subject() {
    let (_c, url) = start_nats().await;
    let client = async_nats::connect(&url).await.unwrap();
    let js = jetstream::new(client);

    // No DLQ stream created — publish should fail at pub-ack time
    let mut h = async_nats::HeaderMap::new();
    h.insert("Nats-Msg-Id", "test-id");
    let result = publish_to_dlq(&js, "dlq.NONEXISTENT.x", Bytes::from_static(b"x"), h).await;

    assert!(result.is_err(), "expected publish to fail when no stream captures subject");
}

#[tokio::test]
async fn publish_to_dlq_dedups_via_nats_msg_id() {
    let (_c, url) = start_nats().await;
    let client = async_nats::connect(&url).await.unwrap();
    let js = jetstream::new(client);
    let cfg = DlqConfig { num_replicas: 1, ..Default::default() };

    ensure_dlq_stream(&js, "DLQ_X_w", "dlq.X.w", &cfg).await.unwrap();

    let info = FailureInfo {
        original_subject: "events.x".into(),
        original_stream:  "X".into(),
        original_seq:     1,
        original_msg_id:  "m1".into(),
        consumer:         "w".into(),
        delivered:        5,
        failure_reason:   "permanent".into(),
        failure_class:    "permanent".into(),
        failure_detail:   "".into(),
    };

    publish_to_dlq(&js, "dlq.X.w", Bytes::from_static(b"first"), build_dlq_headers(&info)).await.unwrap();
    publish_to_dlq(&js, "dlq.X.w", Bytes::from_static(b"second"), build_dlq_headers(&info)).await.unwrap();

    let stream = js.get_stream("DLQ_X_w").await.unwrap();
    let info = stream.get_info().await.unwrap();
    assert_eq!(info.state.messages, 1, "duplicate Nats-Msg-Id must be deduped");
}
```

- [ ] **Step 2: Run tests to verify they fail**

```
cd crates/bus-nats && cargo test --test dlq_test publish_to_dlq
```

Expected: compile error — `publish_to_dlq` not found.

- [ ] **Step 3: Implement `publish_to_dlq`**

Append to `crates/bus-nats/src/dlq.rs`:

```rust
use bytes::Bytes;

/// Atomically publish a message to the DLQ and wait for the JetStream pub-ack.
///
/// Returns `Ok(())` only after the server confirms the message has been
/// persisted in the DLQ stream. Callers MUST treat `Err` as "DLQ entry not
/// stored" and avoid acking/terming the original message — instead, NAK
/// for retry so no data is lost.
pub async fn publish_to_dlq(
    js:      &jetstream::Context,
    subject: &str,
    payload: Bytes,
    headers: HeaderMap,
) -> Result<(), BusError> {
    let ack_fut = js
        .publish_with_headers(subject.to_string(), headers, payload)
        .await
        .map_err(|e| BusError::Nats(format!("dlq publish send: {e}")))?;

    ack_fut
        .await
        .map_err(|e| BusError::Nats(format!("dlq publish ack: {e}")))?;

    Ok(())
}
```

Add `pub use dlq::publish_to_dlq;` to `crates/bus-nats/src/lib.rs`.

- [ ] **Step 4: Run tests to verify they pass**

```
cd crates/bus-nats && cargo test --test dlq_test publish_to_dlq
```

Expected: 3 passed.

- [ ] **Step 5: Commit**

```
git add crates/bus-nats/src/dlq.rs crates/bus-nats/src/lib.rs crates/bus-nats/tests/dlq_test.rs
git commit -m "feat(bus-nats): add atomic publish_to_dlq with pub-ack wait"
```

---

## Task 5: Replace `dlq_subject: Option<String>` with `dlq: Option<DlqOptions>` in SubscribeOptions

**Files:**
- Modify: `crates/bus-nats/src/subscriber.rs`
- Modify: `crates/bus-nats/tests/subscriber_test.rs` (if it constructs `SubscribeOptions { dlq_subject, .. }` — check first)

This is a refactor task — no behavior change yet. The new field is unused until Task 6.

- [ ] **Step 1: Inspect existing usages**

```
cd /Users/olivier/Documents/1hoodlabs/eventbus-rs && grep -rn "dlq_subject" --include="*.rs"
```

Expected: only references in `crates/bus-nats/src/subscriber.rs`.

- [ ] **Step 2: Update `SubscribeOptions`**

In `crates/bus-nats/src/subscriber.rs` lines 16-25, replace:

```rust
pub struct SubscribeOptions {
    pub stream:      String,
    pub durable:     String,
    pub filter:      String,
    pub max_deliver: i64,
    pub ack_wait:    Duration,
    pub backoff:     Vec<Duration>,
    pub concurrency: usize,
    pub dlq_subject: Option<String>,
}
```

with:

```rust
use crate::dlq::DlqOptions;

pub struct SubscribeOptions {
    pub stream:      String,
    pub durable:     String,
    pub filter:      String,
    pub max_deliver: i64,
    pub ack_wait:    Duration,
    pub backoff:     Vec<Duration>,
    pub concurrency: usize,
    /// When set, terminal failures publish to a per-consumer DLQ stream
    /// before terming the source message. The DLQ stream is named
    /// `DLQ_<stream>_<durable>` and listens on `dlq.<stream>.<durable>`.
    pub dlq:         Option<DlqOptions>,
}
```

Update `Default` impl (lines 27-45): replace `dlq_subject: None,` with `dlq: None,`.

In the body of `subscribe()` (line 86), replace:

```rust
let dlq_subject = opts.dlq_subject.clone();
```

with:

```rust
let dlq_opts = opts.dlq.clone();
```

In the `tokio::spawn` body (line 110), replace:

```rust
let dlq = dlq_subject.clone();
```

with:

```rust
let dlq = dlq_opts.clone();
```

In `process_message` signature (line 127) replace `dlq_subject: Option<String>` with `dlq_opts: Option<DlqOptions>` and rename inside the function body.

Update `process_message` signature to receive source stream name + durable (needed to compute DLQ subject in Task 6):

```rust
async fn process_message<E, H, I>(
    msg:      async_nats::jetstream::Message,
    handler:  Arc<H>,
    store:    Arc<I>,
    dlq_opts: Option<DlqOptions>,
    js:       async_nats::jetstream::Context,
    source:   String,   // NEW
    durable:  String,   // NEW
) where
    E: Event,
    H: EventHandler<E>,
    I: IdempotencyStore + ?Sized,
```

In `subscribe()`, before the `tokio::spawn` outer loop, capture:

```rust
let source = opts.stream.clone();
let durable = opts.durable.clone();
```

Then inside `tokio::spawn` for each message, pass them through:

```rust
let source = source.clone();
let durable = durable.clone();
// ...
tokio::spawn(async move {
    let _permit = permit;
    process_message::<E, H, I>(msg, handler, store, dlq, js, source, durable).await;
});
```

Replace the existing `Permanent` arm (lines 192-198) with a temporary atomic-but-naive version that already uses `publish_to_dlq` (Task 6 only refines headers/error handling; this step gets compilation working with the new field):

```rust
Err(HandlerError::Permanent(reason)) => {
    tracing::error!(%msg_id, %reason, "permanent error — terminating");
    if dlq_opts.is_some() {
        let subject = crate::dlq::dlq_subject(&source, &durable);
        let mut headers = async_nats::HeaderMap::new();
        headers.insert(
            crate::dlq::HDR_NATS_MSG_ID,
            format!("{}:{}:{}:{}", source, info.stream_sequence, durable, info.delivered).as_str(),
        );
        let _ = crate::dlq::publish_to_dlq(&js, &subject, msg.payload.clone(), headers).await;
    }
    let _ = ack::term(&msg).await;
}
```

This is intentionally minimal — Task 6 replaces it with full enrichment + NAK-on-failure semantics.

- [ ] **Step 3: Run all tests to verify nothing regressed**

```
cd /Users/olivier/Documents/1hoodlabs/eventbus-rs && cargo test -p bus-nats
```

Expected: all existing tests still pass (no behavior change).

- [ ] **Step 4: Commit**

```
git add crates/bus-nats/src/subscriber.rs
git commit -m "refactor(bus-nats): replace dlq_subject string with structured DlqOptions"
```

---

## Task 6: Fix `Permanent` failure path with atomic publish-then-term

**Files:**
- Modify: `crates/bus-nats/src/subscriber.rs`
- Test: `crates/bus-nats/tests/dlq_test.rs`

- [ ] **Step 1: Write failing integration test**

Append to `crates/bus-nats/tests/dlq_test.rs`:

```rust
use async_trait::async_trait;
use bus_core::{EventHandler, HandlerCtx, HandlerError, MessageId};
use bus_macros::Event;
use bus_nats::subscriber::subscribe;
use bus_nats::{
    NatsClient, NatsKvIdempotencyStore, NatsPublisher, StreamConfig, SubscribeOptions,
};
use bus_core::Publisher;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Serialize, Deserialize, Event)]
#[event(subject = "events.dlq.test")]
struct DlqTestEvent {
    id:    MessageId,
    value: u32,
}

struct AlwaysPermanent;

#[async_trait]
impl EventHandler<DlqTestEvent> for AlwaysPermanent {
    async fn handle(&self, _ctx: HandlerCtx, _evt: DlqTestEvent) -> Result<(), HandlerError> {
        Err(HandlerError::Permanent("invalid card".into()))
    }
}

#[tokio::test]
async fn permanent_error_publishes_to_dlq_with_enriched_headers() {
    let (_c, url) = start_nats().await;
    let cfg = StreamConfig { num_replicas: 1, ..Default::default() };
    let client = NatsClient::connect(&url, &cfg).await.unwrap();
    let publisher = NatsPublisher::new(client.clone());
    let store = Arc::new(
        NatsKvIdempotencyStore::new(client.jetstream().clone(), Duration::from_secs(60))
            .await.unwrap(),
    );

    // Pre-create DLQ stream
    let dlq_cfg = DlqConfig { num_replicas: 1, ..Default::default() };
    ensure_dlq_stream(
        client.jetstream(),
        "DLQ_EVENTS_dlq-test",
        "dlq.EVENTS.dlq-test",
        &dlq_cfg,
    ).await.unwrap();

    let opts = SubscribeOptions {
        durable: "dlq-test".into(),
        filter: "events.dlq.>".into(),
        max_deliver: 3,
        dlq: Some(DlqOptions { config: dlq_cfg.clone() }),
        ..Default::default()
    };

    let _h = subscribe::<DlqTestEvent, _, _>(client.clone(), opts, Arc::new(AlwaysPermanent), store)
        .await.unwrap();

    let evt = DlqTestEvent { id: MessageId::new(), value: 1 };
    publisher.publish(&evt).await.unwrap();

    tokio::time::sleep(Duration::from_secs(2)).await;

    let dlq_stream = client.jetstream().get_stream("DLQ_EVENTS_dlq-test").await.unwrap();
    let info = dlq_stream.get_info().await.unwrap();
    assert_eq!(info.state.messages, 1, "DLQ must have exactly 1 entry");

    let raw = dlq_stream.direct_get_first_for_subject("dlq.EVENTS.dlq-test").await.unwrap();
    let h = raw.headers.unwrap();
    assert_eq!(h.get("X-Failure-Class").unwrap().as_str(), "permanent");
    assert_eq!(h.get("X-Failure-Reason").unwrap().as_str(), "handler_permanent");
    assert_eq!(h.get("X-Failure-Detail").unwrap().as_str(), "invalid card");
    assert_eq!(h.get("X-Consumer").unwrap().as_str(), "dlq-test");
    assert_eq!(h.get("X-Original-Stream").unwrap().as_str(), "EVENTS");
}
```

- [ ] **Step 2: Run test to verify it fails**

```
cd crates/bus-nats && cargo test --test dlq_test permanent_error_publishes_to_dlq
```

Expected: FAIL — message count likely 0 (no DLQ entry) or assertion fails on missing headers.

- [ ] **Step 3: Implement atomic publish-then-term in `Permanent` branch**

In `crates/bus-nats/src/subscriber.rs`, replace the `Permanent` arm (the temporary placeholder added in Task 5) with:

```rust
Err(HandlerError::Permanent(reason)) => {
    tracing::error!(%msg_id, %reason, "permanent error — publishing to DLQ");
    if let Some(_dlq) = &dlq_opts {
        let info_data = crate::dlq::FailureInfo {
            original_subject: msg.subject.to_string(),
            original_stream:  source.clone(),
            original_seq:     info.stream_sequence,
            original_msg_id:  msg_id.to_string(),
            consumer:         durable.clone(),
            delivered:        info.delivered as u64,
            failure_reason:   crate::dlq::REASON_HANDLER_PERMANENT.into(),
            failure_class:    crate::dlq::CLASS_PERMANENT.into(),
            failure_detail:   reason.clone(),
        };
        let headers = crate::dlq::build_dlq_headers(&info_data);
        let subject = crate::dlq::dlq_subject(&source, &durable);

        match crate::dlq::publish_to_dlq(&js, &subject, msg.payload.clone(), headers).await {
            Ok(()) => {
                let _ = ack::term(&msg).await;
            }
            Err(e) => {
                tracing::error!(%msg_id, "DLQ publish failed: {} — NAKing for retry", e);
                let _ = ack::nak_with_delay(&msg, crate::dlq::DLQ_FAILURE_NAK_DELAY).await;
            }
        }
    } else {
        // No DLQ configured — preserve old behavior (term without DLQ)
        let _ = ack::term(&msg).await;
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

```
cd crates/bus-nats && cargo test --test dlq_test permanent_error_publishes_to_dlq
```

Expected: PASS.

- [ ] **Step 5: Run full bus-nats test suite to ensure no regression**

```
cd /Users/olivier/Documents/1hoodlabs/eventbus-rs && cargo test -p bus-nats
```

Expected: all green.

- [ ] **Step 6: Commit**

```
git add crates/bus-nats/src/subscriber.rs crates/bus-nats/tests/dlq_test.rs
git commit -m "feat(bus-nats): atomic publish-then-term for HandlerError::Permanent"
```

---

## Task 7: Add deserialize-failure DLQ path (poison pill handling)

**Files:**
- Modify: `crates/bus-nats/src/subscriber.rs`
- Test: `crates/bus-nats/tests/dlq_test.rs`

Currently, deserialize failures call `ack::term(&msg)` directly with no DLQ entry — that's silent data loss for poison-pill messages.

- [ ] **Step 1: Write failing integration test**

Append to `crates/bus-nats/tests/dlq_test.rs`:

```rust
#[tokio::test]
async fn deserialize_failure_publishes_to_dlq_as_poison() {
    let (_c, url) = start_nats().await;
    let cfg = StreamConfig { num_replicas: 1, ..Default::default() };
    let client = NatsClient::connect(&url, &cfg).await.unwrap();
    let store = Arc::new(
        NatsKvIdempotencyStore::new(client.jetstream().clone(), Duration::from_secs(60))
            .await.unwrap(),
    );
    let dlq_cfg = DlqConfig { num_replicas: 1, ..Default::default() };
    ensure_dlq_stream(
        client.jetstream(),
        "DLQ_EVENTS_poison-test",
        "dlq.EVENTS.poison-test",
        &dlq_cfg,
    ).await.unwrap();

    let opts = SubscribeOptions {
        durable: "poison-test".into(),
        filter: "events.poison.>".into(),
        max_deliver: 3,
        dlq: Some(DlqOptions { config: dlq_cfg }),
        ..Default::default()
    };

    // Subscriber expects DlqTestEvent shape but we'll publish garbage
    let _h = subscribe::<DlqTestEvent, _, _>(
        client.clone(), opts, Arc::new(AlwaysPermanent), store
    ).await.unwrap();

    // Publish raw garbage to events.poison.x — won't deserialize
    let raw_client = async_nats::connect(&url).await.unwrap();
    let js = jetstream::new(raw_client);
    let mut h = async_nats::HeaderMap::new();
    h.insert("Nats-Msg-Id", "poison-1");
    let ack = js.publish_with_headers("events.poison.x", h, Bytes::from_static(b"NOT JSON"))
        .await.unwrap();
    ack.await.unwrap();

    tokio::time::sleep(Duration::from_secs(2)).await;

    let dlq_stream = client.jetstream().get_stream("DLQ_EVENTS_poison-test").await.unwrap();
    let info = dlq_stream.get_info().await.unwrap();
    assert_eq!(info.state.messages, 1);

    let raw = dlq_stream.direct_get_first_for_subject("dlq.EVENTS.poison-test").await.unwrap();
    let h = raw.headers.unwrap();
    assert_eq!(h.get("X-Failure-Class").unwrap().as_str(), "poison");
    assert_eq!(h.get("X-Failure-Reason").unwrap().as_str(), "invalid_payload");
    assert_eq!(raw.payload.as_ref(), b"NOT JSON");
}
```

- [ ] **Step 2: Run test to verify it fails**

```
cd crates/bus-nats && cargo test --test dlq_test deserialize_failure
```

Expected: FAIL — DLQ has 0 messages (currently just terms without DLQ).

- [ ] **Step 3: Implement DLQ path for deserialize failure**

In `crates/bus-nats/src/subscriber.rs`, find the deserialize block (lines 165-173):

```rust
// Deserialize event
let event: E = match serde_json::from_slice(&msg.payload) {
    Ok(e) => e,
    Err(e) => {
        tracing::error!(%msg_id, "failed to deserialize event: {} — terminating", e);
        let _ = ack::term(&msg).await;
        return;
    }
};
```

Replace with:

```rust
let event: E = match serde_json::from_slice(&msg.payload) {
    Ok(e) => e,
    Err(e) => {
        tracing::error!(%msg_id, "failed to deserialize event: {} — sending to DLQ", e);
        if let Some(_dlq) = &dlq_opts {
            let info_data = crate::dlq::FailureInfo {
                original_subject: msg.subject.to_string(),
                original_stream:  source.clone(),
                original_seq:     info.stream_sequence,
                original_msg_id:  msg_id.to_string(),
                consumer:         durable.clone(),
                delivered:        info.delivered as u64,
                failure_reason:   crate::dlq::REASON_INVALID_PAYLOAD.into(),
                failure_class:    crate::dlq::CLASS_POISON.into(),
                failure_detail:   e.to_string(),
            };
            let headers = crate::dlq::build_dlq_headers(&info_data);
            let subject = crate::dlq::dlq_subject(&source, &durable);

            match crate::dlq::publish_to_dlq(&js, &subject, msg.payload.clone(), headers).await {
                Ok(()) => {
                    let _ = ack::term(&msg).await;
                }
                Err(err) => {
                    tracing::error!(%msg_id, "DLQ publish failed: {} — NAKing", err);
                    let _ = ack::nak_with_delay(&msg, crate::dlq::DLQ_FAILURE_NAK_DELAY).await;
                }
            }
        } else {
            let _ = ack::term(&msg).await;
        }
        return;
    }
};
```

- [ ] **Step 4: Run test to verify it passes**

```
cd crates/bus-nats && cargo test --test dlq_test deserialize_failure
```

Expected: PASS.

- [ ] **Step 5: Commit**

```
git add crates/bus-nats/src/subscriber.rs crates/bus-nats/tests/dlq_test.rs
git commit -m "feat(bus-nats): publish poison-pill messages to DLQ before term"
```

---

## Task 8: Add MaxDeliver-on-Transient final-attempt DLQ path

**Files:**
- Modify: `crates/bus-nats/src/subscriber.rs`
- Test: `crates/bus-nats/tests/dlq_test.rs`

When a handler returns `Transient` repeatedly until `MaxDeliver` is hit, the current code just NAKs forever — server eventually stops redelivering and the message is "lost" silently (still in source stream, but the consumer never picks it up again).

- [ ] **Step 1: Write failing integration test**

Append to `crates/bus-nats/tests/dlq_test.rs`:

```rust
struct AlwaysTransient(Arc<AtomicU32>);

#[async_trait]
impl EventHandler<DlqTestEvent> for AlwaysTransient {
    async fn handle(&self, _ctx: HandlerCtx, _evt: DlqTestEvent) -> Result<(), HandlerError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(HandlerError::Transient("db down".into()))
    }
}

use std::sync::atomic::{AtomicU32, Ordering};

#[tokio::test]
async fn transient_exhausting_max_deliver_publishes_to_dlq() {
    let (_c, url) = start_nats().await;
    let cfg = StreamConfig { num_replicas: 1, ..Default::default() };
    let client = NatsClient::connect(&url, &cfg).await.unwrap();
    let publisher = NatsPublisher::new(client.clone());
    let store = Arc::new(
        NatsKvIdempotencyStore::new(client.jetstream().clone(), Duration::from_secs(60))
            .await.unwrap(),
    );
    let dlq_cfg = DlqConfig { num_replicas: 1, ..Default::default() };
    ensure_dlq_stream(
        client.jetstream(),
        "DLQ_EVENTS_transient-test",
        "dlq.EVENTS.transient-test",
        &dlq_cfg,
    ).await.unwrap();

    let counter = Arc::new(AtomicU32::new(0));
    let handler = Arc::new(AlwaysTransient(counter.clone()));

    let opts = SubscribeOptions {
        durable: "transient-test".into(),
        filter: "events.dlq.>".into(),
        max_deliver: 3,
        ack_wait: Duration::from_secs(1),
        backoff: vec![Duration::from_millis(100); 3],
        dlq: Some(DlqOptions { config: dlq_cfg }),
        ..Default::default()
    };

    let _h = subscribe::<DlqTestEvent, _, _>(client.clone(), opts, handler, store)
        .await.unwrap();

    publisher.publish(&DlqTestEvent { id: MessageId::new(), value: 1 }).await.unwrap();

    // Wait for max_deliver=3 attempts
    tokio::time::sleep(Duration::from_secs(3)).await;

    assert_eq!(counter.load(Ordering::SeqCst), 3, "handler must be invoked exactly max_deliver times");

    let dlq_stream = client.jetstream().get_stream("DLQ_EVENTS_transient-test").await.unwrap();
    let dlq_info = dlq_stream.get_info().await.unwrap();
    assert_eq!(dlq_info.state.messages, 1, "DLQ must have entry after MaxDeliver exhaustion");

    let raw = dlq_stream.direct_get_first_for_subject("dlq.EVENTS.transient-test").await.unwrap();
    let h = raw.headers.unwrap();
    assert_eq!(h.get("X-Failure-Class").unwrap().as_str(), "transient_exhausted");
    assert_eq!(h.get("X-Failure-Reason").unwrap().as_str(), "max_retries_exceeded");
    assert_eq!(h.get("X-Retry-Count").unwrap().as_str(), "3");
}
```

- [ ] **Step 2: Run test to verify it fails**

```
cd crates/bus-nats && cargo test --test dlq_test transient_exhausting
```

Expected: FAIL — DLQ has 0 messages.

- [ ] **Step 3: Implement final-attempt detection in `Transient` branch**

In `crates/bus-nats/src/subscriber.rs`, the `Transient` branch is currently:

```rust
Err(HandlerError::Transient(reason)) => {
    tracing::warn!(%msg_id, %reason, "transient error — NAKing with backoff");
    let _ = ack::nak_with_delay(&msg, Duration::from_secs(5)).await;
}
```

We need to capture `max_deliver` and pass it through. First, add it to `process_message` signature:

```rust
async fn process_message<E, H, I>(
    msg:         async_nats::jetstream::Message,
    handler:     Arc<H>,
    store:       Arc<I>,
    dlq_opts:    Option<DlqOptions>,
    js:          async_nats::jetstream::Context,
    source:      String,
    durable:     String,
    max_deliver: i64,           // NEW
    backoff:     Vec<Duration>, // NEW (used in Task 9)
) where
```

And pass them in the spawn loop:

```rust
let max_deliver = opts.max_deliver;
let backoff = opts.backoff.clone();
// ...
process_message::<E, H, I>(
    msg, handler, store, dlq, js, source.clone(), durable.clone(),
    max_deliver, backoff.clone(),
).await;
```

Then replace the `Transient` arm with:

```rust
Err(HandlerError::Transient(reason)) => {
    let attempt = info.delivered as i64;
    let is_final = max_deliver > 0 && attempt >= max_deliver;

    if is_final {
        tracing::error!(%msg_id, %reason, attempt, "transient error on final attempt — sending to DLQ");
        if let Some(_dlq) = &dlq_opts {
            let info_data = crate::dlq::FailureInfo {
                original_subject: msg.subject.to_string(),
                original_stream:  source.clone(),
                original_seq:     info.stream_sequence,
                original_msg_id:  msg_id.to_string(),
                consumer:         durable.clone(),
                delivered:        info.delivered as u64,
                failure_reason:   crate::dlq::REASON_MAX_RETRIES_EXCEEDED.into(),
                failure_class:    crate::dlq::CLASS_TRANSIENT_EXHAUSTED.into(),
                failure_detail:   reason.clone(),
            };
            let headers = crate::dlq::build_dlq_headers(&info_data);
            let subject = crate::dlq::dlq_subject(&source, &durable);

            match crate::dlq::publish_to_dlq(&js, &subject, msg.payload.clone(), headers).await {
                Ok(()) => {
                    let _ = ack::term(&msg).await;
                }
                Err(err) => {
                    tracing::error!(%msg_id, "DLQ publish failed on final attempt: {} — NAKing", err);
                    let _ = ack::nak_with_delay(&msg, crate::dlq::DLQ_FAILURE_NAK_DELAY).await;
                }
            }
        } else {
            let _ = ack::term(&msg).await;
        }
    } else {
        // Task 9 replaces this with compute_backoff(&backoff, attempt)
        tracing::warn!(%msg_id, %reason, attempt, "transient error — NAKing");
        let _ = ack::nak_with_delay(&msg, crate::dlq::FALLBACK_NAK_DELAY).await;
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

```
cd crates/bus-nats && cargo test --test dlq_test transient_exhausting
```

Expected: PASS.

- [ ] **Step 5: Run full bus-nats test suite**

```
cargo test -p bus-nats
```

Expected: all green.

- [ ] **Step 6: Commit**

```
git add crates/bus-nats/src/subscriber.rs crates/bus-nats/tests/dlq_test.rs
git commit -m "feat(bus-nats): publish to DLQ when transient handler exhausts max_deliver"
```

---

## Task 9: Use backoff array for Transient nak delay (honor consumer backoff)

**Files:**
- Modify: `crates/bus-nats/src/subscriber.rs`
- Test: `crates/bus-nats/tests/dlq_test.rs`

NATS server's `BackOff` consumer config only applies to AckWait timeouts, not explicit NAKs. To honor the configured backoff array on Transient NAKs, we must pass the delay explicitly.

- [ ] **Step 1: Write failing test**

Append to `crates/bus-nats/tests/dlq_test.rs`:

```rust
struct TimingHandler(Arc<tokio::sync::Mutex<Vec<std::time::Instant>>>);

#[async_trait]
impl EventHandler<DlqTestEvent> for TimingHandler {
    async fn handle(&self, _ctx: HandlerCtx, _evt: DlqTestEvent) -> Result<(), HandlerError> {
        self.0.lock().await.push(std::time::Instant::now());
        Err(HandlerError::Transient("retrying".into()))
    }
}

#[tokio::test]
async fn transient_nak_uses_configured_backoff() {
    let (_c, url) = start_nats().await;
    let cfg = StreamConfig { num_replicas: 1, ..Default::default() };
    let client = NatsClient::connect(&url, &cfg).await.unwrap();
    let publisher = NatsPublisher::new(client.clone());
    let store = Arc::new(
        NatsKvIdempotencyStore::new(client.jetstream().clone(), Duration::from_secs(60))
            .await.unwrap(),
    );
    let dlq_cfg = DlqConfig { num_replicas: 1, ..Default::default() };
    ensure_dlq_stream(
        client.jetstream(),
        "DLQ_EVENTS_backoff-test",
        "dlq.EVENTS.backoff-test",
        &dlq_cfg,
    ).await.unwrap();

    let timestamps = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let handler = Arc::new(TimingHandler(timestamps.clone()));

    let opts = SubscribeOptions {
        durable: "backoff-test".into(),
        filter: "events.dlq.>".into(),
        max_deliver: 3,
        ack_wait: Duration::from_secs(10),
        backoff: vec![
            Duration::from_millis(200),
            Duration::from_millis(800),
        ],
        dlq: Some(DlqOptions { config: dlq_cfg }),
        ..Default::default()
    };

    let _h = subscribe::<DlqTestEvent, _, _>(client.clone(), opts, handler, store)
        .await.unwrap();

    publisher.publish(&DlqTestEvent { id: MessageId::new(), value: 1 }).await.unwrap();
    tokio::time::sleep(Duration::from_secs(3)).await;

    let ts = timestamps.lock().await;
    assert_eq!(ts.len(), 3);

    let gap1 = ts[1].duration_since(ts[0]);
    let gap2 = ts[2].duration_since(ts[1]);

    assert!(gap1 >= Duration::from_millis(150), "first retry should be ~200ms (got {:?})", gap1);
    assert!(gap1 < Duration::from_millis(600), "first retry should be <600ms (got {:?})", gap1);
    assert!(gap2 >= Duration::from_millis(700), "second retry should be ~800ms (got {:?})", gap2);
}
```

- [ ] **Step 2: Run test to verify it fails**

```
cd crates/bus-nats && cargo test --test dlq_test transient_nak_uses_configured_backoff
```

Expected: FAIL — current hardcoded 5s delay means second invocation never happens within 3s.

- [ ] **Step 3: Implement backoff-indexed nak delay**

In `crates/bus-nats/src/subscriber.rs`, in the non-final `Transient` branch (Task 8 step 3 added the `else` arm):

```rust
} else {
    // Task 9 replaces this with compute_backoff(&backoff, attempt)
    tracing::warn!(%msg_id, %reason, attempt, "transient error — NAKing");
    let _ = ack::nak_with_delay(&msg, crate::dlq::FALLBACK_NAK_DELAY).await;
}
```

Replace with:

```rust
} else {
    let delay = compute_backoff(&backoff, attempt);
    tracing::warn!(%msg_id, %reason, attempt, ?delay, "transient error — NAKing");
    let _ = ack::nak_with_delay(&msg, delay).await;
}
```

Add helper at the bottom of `crates/bus-nats/src/subscriber.rs`:

```rust
/// Pick the backoff delay for the given attempt number (1-indexed from `info.delivered`).
/// Saturates at the last entry. Returns [`crate::dlq::FALLBACK_NAK_DELAY`] if `backoff` is empty.
fn compute_backoff(backoff: &[Duration], attempt: i64) -> Duration {
    if backoff.is_empty() {
        return crate::dlq::FALLBACK_NAK_DELAY;
    }
    let idx = (attempt as usize).saturating_sub(1).min(backoff.len() - 1);
    backoff[idx]
}
```

Add unit test for `compute_backoff` at the bottom of the file:

```rust
#[cfg(test)]
mod tests {
    use super::compute_backoff;
    use std::time::Duration;

    #[test]
    fn compute_backoff_picks_correct_index() {
        let b = vec![
            Duration::from_secs(1),
            Duration::from_secs(5),
            Duration::from_secs(30),
        ];
        assert_eq!(compute_backoff(&b, 1), Duration::from_secs(1));
        assert_eq!(compute_backoff(&b, 2), Duration::from_secs(5));
        assert_eq!(compute_backoff(&b, 3), Duration::from_secs(30));
    }

    #[test]
    fn compute_backoff_saturates_at_last() {
        let b = vec![Duration::from_secs(1), Duration::from_secs(5)];
        assert_eq!(compute_backoff(&b, 99), Duration::from_secs(5));
    }

    #[test]
    fn compute_backoff_empty_returns_fallback() {
        use crate::dlq::FALLBACK_NAK_DELAY;
        assert_eq!(compute_backoff(&[], 1), FALLBACK_NAK_DELAY);
    }

    #[test]
    fn compute_backoff_attempt_zero_clamps_to_first() {
        let b = vec![Duration::from_secs(1)];
        assert_eq!(compute_backoff(&b, 0), Duration::from_secs(1));
    }
}
```

- [ ] **Step 4: Run all tests**

```
cd /Users/olivier/Documents/1hoodlabs/eventbus-rs && cargo test -p bus-nats
```

Expected: all green.

- [ ] **Step 5: Commit**

```
git add crates/bus-nats/src/subscriber.rs crates/bus-nats/tests/dlq_test.rs
git commit -m "feat(bus-nats): use configured backoff array for Transient nak delay"
```

---

## Task 10: Add `EventBusBuilder::with_dlq()` high-level API

**Files:**
- Modify: `crates/event-bus/src/builder.rs`
- Modify: `crates/event-bus/src/bus.rs`
- Modify: `crates/event-bus/src/prelude.rs`
- Test: `crates/event-bus/tests/builder_test.rs`

This is the user-facing API: opt into DLQ once at build time, and every subscribe automatically gets a per-consumer DLQ stream + wiring.

- [ ] **Step 1: Inspect existing builder_test.rs**

```
sed -n '1,40p' crates/event-bus/tests/builder_test.rs
```

Note the existing imports + container-bootstrap helper to reuse.

- [ ] **Step 2: Write failing integration test**

Append to `crates/event-bus/tests/builder_test.rs`:

```rust
use async_trait::async_trait;
use bus_core::{EventHandler, HandlerCtx, HandlerError, MessageId};
use bus_macros::Event;
use bus_nats::{DlqConfig, NatsKvIdempotencyStore, SubscribeOptions};
use event_bus::EventBusBuilder;
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Duration};

#[derive(Debug, Serialize, Deserialize, Event)]
#[event(subject = "events.builder.dlq")]
struct BuilderDlqEvent {
    id:    MessageId,
    value: u32,
}

struct AlwaysPermanentHandler;

#[async_trait]
impl EventHandler<BuilderDlqEvent> for AlwaysPermanentHandler {
    async fn handle(&self, _ctx: HandlerCtx, _evt: BuilderDlqEvent) -> Result<(), HandlerError> {
        Err(HandlerError::Permanent("always fail".into()))
    }
}

#[tokio::test]
async fn builder_with_dlq_auto_wires_per_consumer_dlq_stream() {
    // start_nats() is the existing helper in builder_test.rs — reuse it
    let (_c, url) = start_nats().await;

    // Build idempotency store from a separate connection (matches subscriber_test.rs pattern)
    let raw_client = async_nats::connect(&url).await.unwrap();
    let store = NatsKvIdempotencyStore::new(
        async_nats::jetstream::new(raw_client),
        Duration::from_secs(60),
    ).await.unwrap();

    let bus = EventBusBuilder::new()
        .url(&url)
        .replicas(1)
        .idempotency(store)
        .with_dlq(DlqConfig { num_replicas: 1, ..Default::default() })
        .build()
        .await
        .unwrap();

    let opts = SubscribeOptions {
        durable:     "auto-dlq-test".into(),
        filter:      "events.builder.>".into(),
        max_deliver: 3,
        ..Default::default()
    };

    let _h = bus.subscribe::<BuilderDlqEvent, _>(opts, AlwaysPermanentHandler).await.unwrap();

    // Verify DLQ stream was auto-created
    let raw = async_nats::connect(&url).await.unwrap();
    let js = async_nats::jetstream::new(raw);
    let dlq = js.get_stream("DLQ_EVENTS_auto-dlq-test").await;
    assert!(dlq.is_ok(), "DLQ stream should be auto-created by builder");
}
```

If `start_nats()` does not yet exist in `builder_test.rs`, copy the helper verbatim from `crates/bus-nats/tests/subscriber_test.rs` lines 20-31.

- [ ] **Step 3: Run test to verify it fails**

```
cd crates/event-bus && cargo test --test builder_test builder_with_dlq
```

Expected: compile error — `with_dlq` method not found.

- [ ] **Step 4: Add `with_dlq` to builder**

In `crates/event-bus/src/builder.rs`:

Add field:

```rust
use bus_nats::DlqConfig;

pub struct EventBusBuilder {
    url:         Option<String>,
    stream_cfg:  StreamConfig,
    idempotency: Option<Arc<dyn IdempotencyStore>>,
    sqlite_path: Option<PathBuf>,
    otel:        bool,
    dlq:         Option<DlqConfig>,  // NEW
}
```

Update `new()`:

```rust
pub fn new() -> Self {
    Self {
        url:         None,
        stream_cfg:  StreamConfig::default(),
        idempotency: None,
        sqlite_path: None,
        otel:        false,
        dlq:         None,
    }
}
```

Add the builder method:

```rust
/// Enable DLQ for all subscribers. Each consumer gets its own DLQ stream
/// `DLQ_<source>_<durable>` listening on `dlq.<source>.<durable>`.
pub fn with_dlq(mut self, cfg: DlqConfig) -> Self {
    self.dlq = Some(cfg);
    self
}
```

In `build()`:

```rust
let client = NatsClient::connect(&url, &self.stream_cfg).await?;
Ok(EventBus::new(client, idempotency, self.dlq))
```

- [ ] **Step 5: Update `EventBus` to store dlq config and auto-wire on subscribe**

In `crates/event-bus/src/bus.rs`:

```rust
use bus_nats::{DlqConfig, DlqOptions};

#[derive(Clone)]
pub struct EventBus {
    _client:     NatsClient,
    publisher:   NatsPublisher,
    idempotency: Arc<dyn IdempotencyStore>,
    dlq:         Option<DlqConfig>,  // NEW
}

impl EventBus {
    pub(crate) fn new(
        client:      NatsClient,
        idempotency: Arc<dyn IdempotencyStore>,
        dlq:         Option<DlqConfig>,
    ) -> Self {
        let publisher = NatsPublisher::new(client.clone());
        Self { _client: client, publisher, idempotency, dlq }
    }

    pub async fn subscribe<E, H>(
        &self,
        mut opts: SubscribeOptions,
        handler:  H,
    ) -> Result<SubscriptionHandle, BusError>
    where
        E: Event,
        H: EventHandler<E>,
    {
        // Auto-wire DLQ if configured at builder level and not overridden
        if opts.dlq.is_none() {
            if let Some(cfg) = &self.dlq {
                let stream_name = bus_nats::dlq::dlq_stream_name(&opts.stream, &opts.durable);
                let subject     = bus_nats::dlq::dlq_subject(&opts.stream, &opts.durable);
                bus_nats::dlq::ensure_dlq_stream(
                    self._client.jetstream(),
                    &stream_name,
                    &subject,
                    cfg,
                ).await?;
                opts.dlq = Some(DlqOptions { config: cfg.clone() });
            }
        }

        let handle = subscribe::<E, H, dyn IdempotencyStore>(
            self._client.clone(),
            opts,
            Arc::new(handler),
            self.idempotency.clone(),
        ).await?;
        Ok(SubscriptionHandle(handle))
    }
    // ... rest unchanged
}
```

- [ ] **Step 6: Re-export in prelude**

In `crates/event-bus/src/prelude.rs`:

```rust
pub use bus_nats::{DlqConfig, DlqOptions};
```

- [ ] **Step 7: Run test to verify it passes**

```
cd /Users/olivier/Documents/1hoodlabs/eventbus-rs && cargo test -p event-bus --test builder_test builder_with_dlq
```

Expected: PASS.

- [ ] **Step 8: Run full workspace test suite**

```
cargo test --workspace
```

Expected: all green.

- [ ] **Step 9: Commit**

```
git add crates/event-bus/src/builder.rs crates/event-bus/src/bus.rs crates/event-bus/src/prelude.rs crates/event-bus/tests/builder_test.rs
git commit -m "feat(event-bus): add EventBusBuilder::with_dlq for auto-wired per-consumer DLQ"
```

---

## Task 11: End-to-end DLQ failure-recovery test

**Files:**
- Test: `crates/bus-nats/tests/dlq_test.rs`

Verify that when DLQ publish fails (DLQ stream missing), the original message is NAK'd (not lost) and stays in the source stream.

- [ ] **Step 1: Write failing test**

The strongest signal that NAK happened (rather than a silent Term) is that the handler is invoked exactly `max_deliver` times — Term short-circuits redelivery, NAK preserves it.

Append to `crates/bus-nats/tests/dlq_test.rs`:

```rust
struct CountingPermanent(Arc<AtomicU32>);

#[async_trait]
impl EventHandler<DlqTestEvent> for CountingPermanent {
    async fn handle(&self, _ctx: HandlerCtx, _evt: DlqTestEvent) -> Result<(), HandlerError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(HandlerError::Permanent("always fails".into()))
    }
}

#[tokio::test]
async fn dlq_publish_failure_naks_original_handler_invoked_max_deliver_times() {
    let (_c, url) = start_nats().await;
    let cfg = StreamConfig { num_replicas: 1, ..Default::default() };
    let client = NatsClient::connect(&url, &cfg).await.unwrap();
    let publisher = NatsPublisher::new(client.clone());
    let store = Arc::new(
        NatsKvIdempotencyStore::new(client.jetstream().clone(), Duration::from_secs(60))
            .await.unwrap(),
    );

    // INTENTIONALLY do NOT create DLQ stream — publish_to_dlq will fail with "no streams"
    let dlq_cfg = DlqConfig { num_replicas: 1, ..Default::default() };

    let counter = Arc::new(AtomicU32::new(0));
    let handler = Arc::new(CountingPermanent(counter.clone()));

    let opts = SubscribeOptions {
        durable: "no-dlq-stream".into(),
        filter: "events.dlq.>".into(),
        max_deliver: 3,
        ack_wait: Duration::from_secs(1),
        backoff: vec![Duration::from_millis(100); 3],
        dlq: Some(DlqOptions { config: dlq_cfg }),
        ..Default::default()
    };

    let _h = subscribe::<DlqTestEvent, _, _>(
        client.clone(), opts, handler, store
    ).await.unwrap();

    publisher.publish(&DlqTestEvent { id: MessageId::new(), value: 1 }).await.unwrap();

    // Wait long enough for all retries
    tokio::time::sleep(Duration::from_secs(4)).await;

    // If NAK behavior works correctly, handler is called max_deliver=3 times.
    // If we (incorrectly) Term'd on first DLQ failure, handler would be called only 1 time.
    assert_eq!(
        counter.load(Ordering::SeqCst),
        3,
        "handler must be invoked max_deliver times — NAK preserves redelivery on DLQ failure"
    );
}
```

- [ ] **Step 2: Run test**

```
cd crates/bus-nats && cargo test --test dlq_test dlq_publish_failure_naks_original
```

Expected: PASS (since Task 6 already implements the NAK-on-DLQ-failure path).

- [ ] **Step 3: Commit**

```
git add crates/bus-nats/tests/dlq_test.rs
git commit -m "test(bus-nats): verify DLQ publish failure preserves original (no data loss)"
```

---

## Self-Review Checklist

After completing all tasks:

- [ ] Run `cargo test --workspace` — all tests green
- [ ] Run `cargo clippy --workspace --all-targets -- -D warnings` — no warnings
- [ ] Run `cargo fmt --all --check` — formatting clean
- [ ] Manually verify: a downstream user can opt into DLQ with `EventBusBuilder::with_dlq(DlqConfig::default())` and every subscribe auto-creates a per-consumer DLQ stream
- [ ] Verify no `let _ =` swallowing of DLQ publish errors remains in `subscriber.rs`
- [ ] Search for `dlq_subject` string field — should be 0 hits in src
- [ ] Confirm `bus-nats/src/advisory.rs` is still a stub (Pattern A is v2, not in scope)

## Out of Scope (deferred to v2)

- **Pattern A advisory listener** (`advisory.rs`) — safety net for worker crash mid-handle, will subscribe to `$JS.EVENT.ADVISORY.CONSUMER.MAX_DELIVERIES.>` and `MSG_TERMINATED.>`
- **DLQ inspector/replay tooling** — future CLI or service that reads DLQ streams, evaluates predicates, and republishes to source
- **OpenTelemetry trace propagation through DLQ headers** — extend `build_dlq_headers` with W3C `traceparent`
- **Per-tenant DLQ for multi-tenant deployments** — separate plan when multi-tenancy lands

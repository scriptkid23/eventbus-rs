# DLQ + Subscriber Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Address six issues found in the DLQ Pattern B v1 review: aggressive 1s pub-ack timeout, hardcoded 7-day idempotency TTL, untracked worker tasks (leak on shutdown), `unwrap()` on semaphore, flaky 14s test, and the correctness gap where `IdempotencyStore::try_insert` cannot distinguish *pending* from *done*, allowing handler double-execution on redelivery.

**Architecture:**
- Replace hardcoded duration constants in `dlq.rs` and `subscriber.rs` with configurable fields on `DlqConfig` / `SubscribeOptions`. Defaults preserve current behavior except for the pub-ack timeout (removed — async-nats client already enforces a 5s request timeout).
- Track spawned worker futures with `tokio::task::JoinSet` so dropping `SubscriptionHandle` aborts in-flight handlers cleanly.
- Extend the `IdempotencyStore` trait with a state-aware `try_claim` method returning `ClaimOutcome::{Claimed, AlreadyPending, AlreadyDone}`. Update `NatsKvIdempotencyStore` and `PostgresIdempotencyStore` to read value/status; update subscriber to dispatch on the outcome instead of the `delivered > 1` heuristic.

**Tech Stack:** `async-nats 0.46`, `sqlx`, `tokio` (`JoinSet`), `testcontainers 0.23`, existing `bus-core`/`bus-nats`/`bus-outbox` workspace crates.

**Prerequisite:** DLQ Pattern B v1 (`docs/superpowers/plans/2026-05-01-dlq-pattern-b-v1.md`) merged. Current state of `main` as of 2026-05-01.

---

## File Map

### `crates/bus-core/src/`
- **Modify:** `idempotency.rs` — add `ClaimOutcome` enum and `try_claim` method to `IdempotencyStore` trait, mark `try_insert` deprecated.
- **Modify:** `lib.rs` — re-export `ClaimOutcome`.

### `crates/bus-nats/src/`
- **Modify:** `dlq.rs` — add `publish_ack_timeout: Option<Duration>` and `failure_nak_delay: Duration` to `DlqConfig`; thread them through `publish_to_dlq` (timeout) and to subscriber (nak delay).
- **Modify:** `subscriber.rs` — replace hardcoded idempotency TTL with `SubscribeOptions.idempotency_ttl`; replace `.unwrap()` on semaphore with `.expect()`; track worker tasks via `JoinSet`; switch from `try_insert` to `try_claim` with explicit outcome dispatch.
- **Modify:** `inbox/nats_kv.rs` — implement `try_claim` reading entry value to distinguish pending/done.

### `crates/bus-outbox/src/`
- **Modify:** `inbox_pg.rs` — implement `try_claim` using `INSERT … ON CONFLICT DO NOTHING RETURNING` followed by status read on conflict.

### Tests
- **Modify:** `crates/bus-nats/tests/inbox_kv_test.rs` — add tests for `try_claim` outcomes.
- **Modify:** `crates/bus-nats/tests/dlq_test.rs` — adjust `dlq_publish_failure_naks_…` to use short `failure_nak_delay`; reduce sleep accordingly.
- **Create:** `crates/bus-nats/tests/subscriber_shutdown_test.rs` — verify dropping `SubscriptionHandle` aborts in-flight workers.
- **Create / Modify:** test for double-execution prevention in `dlq_test.rs`.

---

## Task 1: Make DLQ pub-ack timeout and failure nak-delay configurable

**Files:**
- Modify: `crates/bus-nats/src/dlq.rs`
- Modify: `crates/bus-nats/src/subscriber.rs`
- Modify: `crates/bus-nats/tests/dlq_test.rs`

**Why:** The current `DLQ_PUBLISH_ACK_TIMEOUT = 1s` is too aggressive — JetStream cluster ack roundtrip with `num_replicas: 3` can exceed 1s under healthy load (leader election, GC pause). The plan v1 did not specify a timeout; async-nats already enforces a 5s `request_timeout` at the client layer. The current `DLQ_FAILURE_NAK_DELAY = 5s` is also hardcoded, which forces tests to sleep 14s+. Both should be `DlqConfig` fields.

- [ ] **Step 1: Add fields to `DlqConfig`**

In `crates/bus-nats/src/dlq.rs`, replace the existing `DlqConfig` struct (lines 53-62) and its `Default` impl (lines 64-76) with:

```rust
/// Configuration for a per-consumer DLQ stream.
#[derive(Debug, Clone)]
pub struct DlqConfig {
    pub num_replicas: usize,
    pub max_age: Duration,
    pub max_bytes: Option<i64>,
    pub duplicate_window: Duration,
    pub deny_delete: bool,
    pub deny_purge: bool,
    pub allow_direct: bool,
    /// Maximum time to wait for the JetStream pub-ack on a DLQ publish.
    /// `None` (default) defers to the async-nats client request timeout.
    pub publish_ack_timeout: Option<Duration>,
    /// Delay applied via `Nak` on the source message when DLQ publish fails.
    /// Default: [`DEFAULT_DLQ_FAILURE_NAK_DELAY`].
    pub failure_nak_delay: Duration,
}

impl Default for DlqConfig {
    fn default() -> Self {
        Self {
            num_replicas: DEFAULT_DLQ_REPLICAS,
            max_age: DEFAULT_DLQ_MAX_AGE,
            max_bytes: None,
            duplicate_window: DEFAULT_DLQ_DUPLICATE_WINDOW,
            deny_delete: true,
            deny_purge: false,
            allow_direct: true,
            publish_ack_timeout: None,
            failure_nak_delay: DEFAULT_DLQ_FAILURE_NAK_DELAY,
        }
    }
}
```

Rename the constant. Replace line 29:

```rust
pub const DLQ_FAILURE_NAK_DELAY: Duration = Duration::from_secs(5);
```

with:

```rust
pub const DEFAULT_DLQ_FAILURE_NAK_DELAY: Duration = Duration::from_secs(5);
```

Delete line 30 entirely:

```rust
pub const DLQ_PUBLISH_ACK_TIMEOUT: Duration = Duration::from_secs(1);
```

- [ ] **Step 2: Update `publish_to_dlq` to take optional timeout**

Replace the existing `publish_to_dlq` (lines 164-184) with:

```rust
pub async fn publish_to_dlq(
    js: &jetstream::Context,
    subject: &str,
    payload: Bytes,
    headers: HeaderMap,
    publish_ack_timeout: Option<Duration>,
) -> Result<(), BusError> {
    let publish_fut = js.publish_with_headers(subject.to_string(), headers, payload);

    let publish_ack = match publish_ack_timeout {
        Some(timeout) => tokio::time::timeout(timeout, publish_fut)
            .await
            .map_err(|_| BusError::Nats("dlq publish send timeout".to_string()))?,
        None => publish_fut.await,
    }
    .map_err(|error| BusError::Nats(format!("dlq publish send: {error}")))?;

    match publish_ack_timeout {
        Some(timeout) => tokio::time::timeout(timeout, publish_ack)
            .await
            .map_err(|_| BusError::Nats("dlq publish ack timeout".to_string()))?,
        None => publish_ack.await,
    }
    .map_err(|error| BusError::Nats(format!("dlq publish ack: {error}")))?;

    Ok(())
}
```

- [ ] **Step 3: Thread `failure_nak_delay` through subscriber**

In `crates/bus-nats/src/subscriber.rs`, replace the entire `handle_terminal_failure` function (currently lines 300-342) with a version that binds `dlq_opts` so we can read both the timeout and the nak delay from it:

```rust
async fn handle_terminal_failure<I>(
    msg: &Message,
    store: &I,
    processing_options: &ProcessingOptions,
    failure: TerminalFailure<'_>,
) where
    I: IdempotencyStore + ?Sized,
{
    let Some(dlq_opts) = processing_options.dlq_opts.as_ref() else {
        mark_done_and_term(store, msg, failure.msg_id).await;
        return;
    };

    let failure_info = FailureInfo {
        original_subject: msg.subject.to_string(),
        original_stream: processing_options.source.clone(),
        original_seq: failure.stream_sequence,
        original_msg_id: failure.msg_id.to_string(),
        consumer: processing_options.durable.clone(),
        delivered: failure.delivered,
        failure_reason: failure.failure_reason.to_string(),
        failure_class: failure.failure_class.to_string(),
        failure_detail: failure.failure_detail,
    };
    let headers = build_dlq_headers(&failure_info);
    let subject = dlq_subject(&processing_options.source, &processing_options.durable);

    match publish_to_dlq(
        &processing_options.js,
        &subject,
        msg.payload.clone(),
        headers,
        dlq_opts.config.publish_ack_timeout,
    )
    .await
    {
        Ok(()) => mark_done_and_term(store, msg, failure.msg_id).await,
        Err(error) => {
            tracing::error!(
                msg_id = %failure.msg_id,
                "DLQ publish failed: {} — NAKing for retry",
                error,
            );
            release_and_nak(
                store,
                msg,
                failure.msg_id,
                dlq_opts.config.failure_nak_delay,
            )
            .await;
        }
    }
}
```

Update the import block at top of `subscriber.rs` (lines 5-10): drop `DLQ_FAILURE_NAK_DELAY` from the imports.

- [ ] **Step 4: Update `dlq_test.rs` import + the slow test**

In `crates/bus-nats/tests/dlq_test.rs`, update calls to `publish_to_dlq` (lines 292-297, 320, 350-356, 358-363) to add the new `publish_ack_timeout` argument as `None`:

```rust
publish_to_dlq(
    &js,
    "dlq.X.w",
    payload.clone(),
    build_dlq_headers(&failure_info),
    None,
)
.await
.unwrap();
```

Apply the same `None` argument to all three `publish_to_dlq` call sites in this file.

In the test `dlq_publish_failure_naks_original_handler_invoked_max_deliver_times` (lines 662-709), set a short `failure_nak_delay` and reduce the sleep:

```rust
let options = SubscribeOptions {
    durable: "no-dlq-stream".into(),
    filter: "events.dlq.>".into(),
    max_deliver: 3,
    ack_wait: Duration::from_secs(1),
    backoff: vec![Duration::from_millis(100), Duration::from_millis(100)],
    dlq: Some(DlqOptions {
        config: DlqConfig {
            num_replicas: 1,
            failure_nak_delay: Duration::from_millis(200),
            publish_ack_timeout: Some(Duration::from_millis(500)),
            ..Default::default()
        },
    }),
    ..Default::default()
};
```

Reduce the sleep from `Duration::from_secs(14)` to `Duration::from_secs(6)`.

- [ ] **Step 5: Run tests**

```
cd /Users/olivier/Documents/1hoodlabs/eventbus-rs && cargo test -p bus-nats --test dlq_test
```

Expected: all 12 DLQ tests pass.

- [ ] **Step 6: Commit**

```
git add crates/bus-nats/src/dlq.rs crates/bus-nats/src/subscriber.rs crates/bus-nats/tests/dlq_test.rs
git commit -m "feat(bus-nats): make DLQ pub-ack timeout and failure nak-delay configurable"
```

---

## Task 2: Make idempotency TTL configurable on `SubscribeOptions`

**Files:**
- Modify: `crates/bus-nats/src/subscriber.rs`

**Why:** The current `Duration::from_secs(86400 * 7)` literal at line 188 is a magic number; it should be a typed constant and overridable per-subscription.

- [ ] **Step 1: Add constant + field**

In `crates/bus-nats/src/subscriber.rs`, add at the top of the file just below the existing imports (before `pub struct SubscribeOptions`):

```rust
const SECONDS_PER_DAY: u64 = 24 * 60 * 60;

pub const DEFAULT_IDEMPOTENCY_TTL: Duration = Duration::from_secs(7 * SECONDS_PER_DAY);
```

Add the field to `SubscribeOptions` (replace lines 29-38 with the body augmented to include the new field):

```rust
pub struct SubscribeOptions {
    pub stream: String,
    pub durable: String,
    pub filter: String,
    pub max_deliver: i64,
    pub ack_wait: Duration,
    pub backoff: Vec<Duration>,
    pub concurrency: usize,
    pub dlq: Option<DlqOptions>,
    /// TTL for idempotency keys claimed by `try_claim`. Default: [`DEFAULT_IDEMPOTENCY_TTL`].
    pub idempotency_ttl: Duration,
}
```

Add `idempotency_ttl: DEFAULT_IDEMPOTENCY_TTL,` to the `Default` impl (the body currently spans lines 41-57).

- [ ] **Step 2: Use the field at call site**

In the same file, replace line 188:

```rust
.try_insert(&msg_id, Duration::from_secs(86400 * 7))
```

with:

```rust
.try_insert(&msg_id, processing_options.idempotency_ttl)
```

Add `idempotency_ttl: Duration` to the `ProcessingOptions` struct (currently lines 65-73):

```rust
#[derive(Clone)]
struct ProcessingOptions {
    dlq_opts: Option<DlqOptions>,
    js: jetstream::Context,
    source: String,
    durable: String,
    max_deliver: i64,
    backoff: Vec<Duration>,
    idempotency_ttl: Duration,
}
```

Populate it in `subscribe()` (currently lines 118-125):

```rust
let processing_options = ProcessingOptions {
    dlq_opts: opts.dlq.clone(),
    js: client.js.clone(),
    source: opts.stream.clone(),
    durable: opts.durable.clone(),
    max_deliver: opts.max_deliver,
    backoff: opts.backoff.clone(),
    idempotency_ttl: opts.idempotency_ttl,
};
```

- [ ] **Step 3: Run tests**

```
cd /Users/olivier/Documents/1hoodlabs/eventbus-rs && cargo test -p bus-nats
```

Expected: all green. No behavior change — default TTL identical to previous magic number.

- [ ] **Step 4: Commit**

```
git add crates/bus-nats/src/subscriber.rs
git commit -m "refactor(bus-nats): make idempotency TTL configurable on SubscribeOptions"
```

---

## Task 3: Replace `unwrap` on semaphore with explicit `expect`

**Files:**
- Modify: `crates/bus-nats/src/subscriber.rs`

**Why:** `acquire_owned` only fails when the semaphore is closed. We never close it, so the `Result` cannot be `Err` — but `.unwrap()` reads as "unhandled". `.expect()` documents the invariant.

- [ ] **Step 1: Edit the call**

In `crates/bus-nats/src/subscriber.rs` line 145, replace:

```rust
let permit = semaphore.clone().acquire_owned().await.unwrap();
```

with:

```rust
let permit = semaphore
    .clone()
    .acquire_owned()
    .await
    .expect("subscriber semaphore is never closed");
```

- [ ] **Step 2: Run tests**

```
cd /Users/olivier/Documents/1hoodlabs/eventbus-rs && cargo test -p bus-nats
```

Expected: all green.

- [ ] **Step 3: Commit**

```
git add crates/bus-nats/src/subscriber.rs
git commit -m "chore(bus-nats): document semaphore invariant with expect"
```

---

## Task 4: Track worker tasks via `JoinSet` and abort on shutdown

**Files:**
- Modify: `crates/bus-nats/src/subscriber.rs`
- Create: `crates/bus-nats/tests/subscriber_shutdown_test.rs`

**Why:** Today, dropping `SubscriptionHandle` aborts only the outer message-loop task. Each per-message worker is a detached `tokio::spawn` and continues to run, potentially issuing acks/naks/DLQ publishes on a connection that is about to close. A `JoinSet` owned by the outer task aborts every worker when the outer task drops it.

- [ ] **Step 1: Write failing integration test**

Create `crates/bus-nats/tests/subscriber_shutdown_test.rs`:

```rust
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
```

- [ ] **Step 2: Run test to verify it fails**

```
cd /Users/olivier/Documents/1hoodlabs/eventbus-rs && cargo test -p bus-nats --test subscriber_shutdown_test
```

Expected: FAIL — at least one handler completes (`completed > 0`) because detached workers continue past handle drop.

- [ ] **Step 3: Adopt `JoinSet`**

In `crates/bus-nats/src/subscriber.rs`, update the `tokio::spawn` block in `subscribe()` (currently lines 127-155). Replace it with:

```rust
let handle = tokio::spawn(async move {
    let mut messages = match consumer.messages().await {
        Ok(stream) => stream,
        Err(error) => {
            tracing::error!("failed to get message stream: {}", error);
            return;
        }
    };

    let mut workers: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();

    loop {
        tokio::select! {
            biased;
            Some(joined) = workers.join_next(), if !workers.is_empty() => {
                if let Err(error) = joined {
                    if !error.is_cancelled() {
                        tracing::warn!("worker task error: {}", error);
                    }
                }
            }
            next = messages.next() => {
                let Some(item) = next else { break };
                let msg = match item {
                    Ok(message) => message,
                    Err(error) => {
                        tracing::warn!("message stream error: {}", error);
                        continue;
                    }
                };

                let permit = semaphore
                    .clone()
                    .acquire_owned()
                    .await
                    .expect("subscriber semaphore is never closed");
                let handler = handler.clone();
                let store = idempotency_store.clone();
                let processing_options = processing_options.clone();

                workers.spawn(async move {
                    let _permit = permit;
                    process_message::<E, H, I>(msg, handler, store, processing_options).await;
                });
            }
        }
    }

    workers.shutdown().await;
});
```

When the outer task is aborted (because `SubscriptionHandle._handle` is dropped), tokio drops `workers`, which aborts every spawned future.

- [ ] **Step 4: Run test to verify it passes**

```
cd /Users/olivier/Documents/1hoodlabs/eventbus-rs && cargo test -p bus-nats --test subscriber_shutdown_test
```

Expected: PASS — `completed == 0` after handle drop.

- [ ] **Step 5: Run the full test suite to ensure no regression**

```
cd /Users/olivier/Documents/1hoodlabs/eventbus-rs && cargo test -p bus-nats
```

Expected: all green.

- [ ] **Step 6: Commit**

```
git add crates/bus-nats/src/subscriber.rs crates/bus-nats/tests/subscriber_shutdown_test.rs
git commit -m "feat(bus-nats): track subscriber workers via JoinSet for clean shutdown"
```

---

## Task 5: Add `ClaimOutcome` to `IdempotencyStore` trait

**Files:**
- Modify: `crates/bus-core/src/idempotency.rs`
- Modify: `crates/bus-core/src/lib.rs`

**Why:** Subscriber currently uses the `delivered > 1` redelivery counter as a proxy for "is the previous attempt still pending or already done?" That heuristic is wrong: a successfully completed message whose `double_ack` failed will be redelivered with `delivered > 1`, and the subscriber will re-run the handler. We need a state-aware claim API.

- [ ] **Step 1: Write failing unit test for trait shape**

In `crates/bus-core/src/idempotency.rs`, before defining the trait change, add a doc-test or unit test that pins the new enum + method shape. Append at the bottom of the file:

```rust
#[cfg(test)]
mod tests {
    use super::ClaimOutcome;

    #[test]
    fn claim_outcome_variants_exist() {
        let _ = ClaimOutcome::Claimed;
        let _ = ClaimOutcome::AlreadyPending;
        let _ = ClaimOutcome::AlreadyDone;
    }

    #[test]
    fn claim_outcome_is_eq_and_debug() {
        assert_eq!(ClaimOutcome::Claimed, ClaimOutcome::Claimed);
        assert_ne!(ClaimOutcome::Claimed, ClaimOutcome::AlreadyDone);
        let _ = format!("{:?}", ClaimOutcome::AlreadyPending);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```
cd /Users/olivier/Documents/1hoodlabs/eventbus-rs && cargo test -p bus-core
```

Expected: FAIL — `ClaimOutcome` not defined.

- [ ] **Step 3: Add enum + extend trait**

Replace the entire `crates/bus-core/src/idempotency.rs` contents with:

```rust
use crate::{error::BusError, id::MessageId};
use async_trait::async_trait;
use std::time::Duration;

/// Result of a `try_claim` call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimOutcome {
    /// First time the key has been seen — caller now owns the pending claim.
    Claimed,
    /// Key already exists in `pending` state; another worker may be holding
    /// the claim, or a previous attempt did not complete. Caller should
    /// proceed only if it can guarantee no concurrent execution (e.g. a
    /// JetStream redelivery to the same consumer).
    AlreadyPending,
    /// Key exists in `done` state; the message was successfully processed
    /// before. Caller MUST treat this as a duplicate and ack without running
    /// the handler.
    AlreadyDone,
}

/// Backend-agnostic idempotency store for deduplicating handler executions.
///
/// Implementations must use atomic operations to remain safe under concurrent
/// delivery.
#[async_trait]
pub trait IdempotencyStore: Send + Sync {
    /// State-aware claim. Atomically inserts the key in `pending` state if
    /// absent, otherwise reports the existing state.
    async fn try_claim(
        &self,
        key: &MessageId,
        ttl: Duration,
    ) -> Result<ClaimOutcome, BusError>;

    /// Mark a previously claimed key as successfully processed.
    async fn mark_done(&self, key: &MessageId) -> Result<(), BusError>;

    /// Release a `pending` claim so the message can be retried.
    async fn release(&self, key: &MessageId) -> Result<(), BusError>;
}

#[cfg(test)]
mod tests {
    use super::ClaimOutcome;

    #[test]
    fn claim_outcome_variants_exist() {
        let _ = ClaimOutcome::Claimed;
        let _ = ClaimOutcome::AlreadyPending;
        let _ = ClaimOutcome::AlreadyDone;
    }

    #[test]
    fn claim_outcome_is_eq_and_debug() {
        assert_eq!(ClaimOutcome::Claimed, ClaimOutcome::Claimed);
        assert_ne!(ClaimOutcome::Claimed, ClaimOutcome::AlreadyDone);
        let _ = format!("{:?}", ClaimOutcome::AlreadyPending);
    }
}
```

Note: `try_insert` is removed entirely — both implementations are updated in Tasks 6 and 7. This is a breaking change to the trait, but only two impls exist in this workspace.

- [ ] **Step 4: Re-export from lib.rs**

Open `crates/bus-core/src/lib.rs` and ensure `ClaimOutcome` is re-exported alongside `IdempotencyStore`. Verify the existing re-export line and add `ClaimOutcome`:

```rust
pub use idempotency::{ClaimOutcome, IdempotencyStore};
```

If the file currently has `pub use idempotency::IdempotencyStore;`, replace it with the line above. If `IdempotencyStore` is re-exported through a glob, add `ClaimOutcome` to the same glob.

- [ ] **Step 5: Run unit tests**

```
cd /Users/olivier/Documents/1hoodlabs/eventbus-rs && cargo test -p bus-core
```

Expected: 2 unit tests pass. Workspace will fail to compile elsewhere because impls have not been updated yet — that is expected and fixed in Tasks 6 and 7.

- [ ] **Step 6: Commit**

```
git add crates/bus-core/src/idempotency.rs crates/bus-core/src/lib.rs
git commit -m "feat(bus-core)!: replace try_insert with state-aware try_claim"
```

(Workspace remains broken until Tasks 6 and 7 land.)

---

## Task 6: Implement `try_claim` on `NatsKvIdempotencyStore`

**Files:**
- Modify: `crates/bus-nats/src/inbox/nats_kv.rs`
- Modify: `crates/bus-nats/tests/inbox_kv_test.rs`

- [ ] **Step 1: Update the test file to exercise `try_claim`**

Replace `crates/bus-nats/tests/inbox_kv_test.rs` contents with:

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

```
cd /Users/olivier/Documents/1hoodlabs/eventbus-rs && cargo test -p bus-nats --test inbox_kv_test
```

Expected: compilation error — `try_claim` not implemented on `NatsKvIdempotencyStore`.

- [ ] **Step 3: Implement `try_claim`**

Replace `crates/bus-nats/src/inbox/nats_kv.rs` contents with:

```rust
use async_nats::jetstream::{self, kv};
use async_trait::async_trait;
use bus_core::{
    error::BusError,
    id::MessageId,
    idempotency::{ClaimOutcome, IdempotencyStore},
};
use bytes::Bytes;
use std::time::Duration;

const KV_PENDING: &[u8] = b"pending";
const KV_DONE: &[u8] = b"done";

/// Idempotency store backed by NATS JetStream Key-Value store.
///
/// Uses `kv::Store::create` for atomic, first-writer-wins claim semantics.
/// On conflict, `try_claim` reads the existing entry to distinguish a still
/// pending claim from a completed one.
#[derive(Clone)]
pub struct NatsKvIdempotencyStore {
    store: kv::Store,
}

impl NatsKvIdempotencyStore {
    pub async fn new(js: jetstream::Context, max_age: Duration) -> Result<Self, BusError> {
        let store = js
            .create_key_value(kv::Config {
                bucket: "eventbus_processed".into(),
                history: 1,
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
    async fn try_claim(
        &self,
        key: &MessageId,
        _ttl: Duration,
    ) -> Result<ClaimOutcome, BusError> {
        let key_str = key.to_string();

        match self
            .store
            .create(&key_str, Bytes::from_static(KV_PENDING))
            .await
        {
            Ok(_) => Ok(ClaimOutcome::Claimed),
            Err(_) => match self
                .store
                .get(&key_str)
                .await
                .map_err(|e| BusError::Idempotency(e.to_string()))?
            {
                Some(value) if value.as_ref() == KV_DONE => Ok(ClaimOutcome::AlreadyDone),
                Some(_) => Ok(ClaimOutcome::AlreadyPending),
                None => Ok(ClaimOutcome::Claimed),
            },
        }
    }

    async fn mark_done(&self, key: &MessageId) -> Result<(), BusError> {
        self.store
            .put(key.to_string(), Bytes::from_static(KV_DONE))
            .await
            .map_err(|e| BusError::Idempotency(e.to_string()))?;
        Ok(())
    }

    async fn release(&self, key: &MessageId) -> Result<(), BusError> {
        self.store
            .purge(key.to_string())
            .await
            .map_err(|e| BusError::Idempotency(e.to_string()))?;
        Ok(())
    }
}
```

The fallback `None => Ok(Claimed)` arm handles the rare race where the key was created and then deleted (e.g. via `release`) between our `create` failure and our `get`. Returning `Claimed` is safe: the next `mark_done` will write a new entry.

- [ ] **Step 4: Run tests to verify they pass**

```
cd /Users/olivier/Documents/1hoodlabs/eventbus-rs && cargo test -p bus-nats --test inbox_kv_test
```

Expected: all 4 tests pass.

- [ ] **Step 5: Commit**

```
git add crates/bus-nats/src/inbox/nats_kv.rs crates/bus-nats/tests/inbox_kv_test.rs
git commit -m "feat(bus-nats): implement state-aware try_claim on NatsKvIdempotencyStore"
```

---

## Task 7: Implement `try_claim` on `PostgresIdempotencyStore`

**Files:**
- Modify: `crates/bus-outbox/src/inbox_pg.rs`

**Why:** `bus-outbox` is part of the workspace and provides the Postgres impl. Until it implements the new trait method, `cargo build` fails for the whole workspace.

- [ ] **Step 1: Update the implementation**

Replace `crates/bus-outbox/src/inbox_pg.rs` contents with:

```rust
use async_trait::async_trait;
use bus_core::{
    error::BusError,
    id::MessageId,
    idempotency::{ClaimOutcome, IdempotencyStore},
};
use sqlx::PgPool;
use std::time::Duration;

/// PostgreSQL-backed idempotency store.
///
/// `try_claim` uses `INSERT … ON CONFLICT DO NOTHING RETURNING 1` to detect
/// first writes atomically. On conflict, a follow-up `SELECT status` reads the
/// existing row.
#[derive(Clone)]
pub struct PostgresIdempotencyStore {
    pool: PgPool,
    consumer: String,
}

impl PostgresIdempotencyStore {
    pub fn new(pool: PgPool, consumer: String) -> Self {
        Self { pool, consumer }
    }
}

#[async_trait]
impl IdempotencyStore for PostgresIdempotencyStore {
    async fn try_claim(
        &self,
        key: &MessageId,
        _ttl: Duration,
    ) -> Result<ClaimOutcome, BusError> {
        let inserted: Option<i32> = sqlx::query_scalar(
            r#"INSERT INTO eventbus_inbox (message_id, consumer, status)
               VALUES ($1, $2, 'pending')
               ON CONFLICT (consumer, message_id) DO NOTHING
               RETURNING 1"#,
        )
        .bind(key.to_string())
        .bind(&self.consumer)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| BusError::Idempotency(e.to_string()))?;

        if inserted.is_some() {
            return Ok(ClaimOutcome::Claimed);
        }

        let status: Option<String> = sqlx::query_scalar(
            "SELECT status FROM eventbus_inbox WHERE consumer = $1 AND message_id = $2",
        )
        .bind(&self.consumer)
        .bind(key.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| BusError::Idempotency(e.to_string()))?;

        match status.as_deref() {
            Some("pending") => Ok(ClaimOutcome::AlreadyPending),
            Some("done") => Ok(ClaimOutcome::AlreadyDone),
            Some(other) => Err(BusError::Idempotency(format!(
                "unexpected status `{other}` in eventbus_inbox"
            ))),
            None => Ok(ClaimOutcome::Claimed),
        }
    }

    async fn mark_done(&self, key: &MessageId) -> Result<(), BusError> {
        sqlx::query(
            "UPDATE eventbus_inbox SET status = 'done' WHERE consumer = $1 AND message_id = $2",
        )
        .bind(&self.consumer)
        .bind(key.to_string())
        .execute(&self.pool)
        .await
        .map_err(|e| BusError::Idempotency(e.to_string()))?;
        Ok(())
    }

    async fn release(&self, key: &MessageId) -> Result<(), BusError> {
        sqlx::query(
            "DELETE FROM eventbus_inbox WHERE consumer = $1 AND message_id = $2 AND status = 'pending'",
        )
        .bind(&self.consumer)
        .bind(key.to_string())
        .execute(&self.pool)
        .await
        .map_err(|e| BusError::Idempotency(e.to_string()))?;
        Ok(())
    }
}
```

- [ ] **Step 2: Run workspace check**

```
cd /Users/olivier/Documents/1hoodlabs/eventbus-rs && cargo check --workspace
```

Expected: clean compile across `bus-core`, `bus-nats`, `bus-outbox`. (Subscriber callsite is updated in Task 8 — that file should still compile because it currently uses `try_insert`, which no longer exists. If `cargo check` fails on `subscriber.rs` here, **stop and proceed to Task 8** — Task 8 fixes the call site.)

If `cargo check` fails only inside `subscriber.rs`, that is expected. Continue to Task 8.

- [ ] **Step 3: Commit**

```
git add crates/bus-outbox/src/inbox_pg.rs
git commit -m "feat(bus-outbox): implement state-aware try_claim on PostgresIdempotencyStore"
```

---

## Task 8: Switch subscriber to `try_claim` outcome dispatch

**Files:**
- Modify: `crates/bus-nats/src/subscriber.rs`
- Modify: `crates/bus-nats/tests/dlq_test.rs`

**Why:** This is the actual correctness fix. The `delivered > 1` heuristic is replaced with explicit `ClaimOutcome` dispatch, so a re-delivered message that was already marked done is acked as duplicate (no double-execution).

- [ ] **Step 1: Write failing integration test for `AlreadyDone` short-circuit**

Append to `crates/bus-nats/tests/dlq_test.rs`. The test deterministically forces the `AlreadyDone` branch by pre-populating the idempotency store with the message-id before publishing — no flakiness from ack-wait races.

```rust
struct CountingHandler {
    counter: Arc<AtomicU32>,
}

#[async_trait]
impl EventHandler<DlqTestEvent> for CountingHandler {
    async fn handle(&self, _ctx: HandlerCtx, _evt: DlqTestEvent) -> Result<(), HandlerError> {
        self.counter.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn already_done_message_is_acked_without_invoking_handler() {
    let (_container, url) = start_nats().await;
    let client = connect_nats_client(&url).await;
    let publisher = NatsPublisher::new(client.clone());
    let store = Arc::new(
        NatsKvIdempotencyStore::new(client.jetstream().clone(), Duration::from_secs(60))
            .await
            .unwrap(),
    );

    // Pre-mark a message-id as done. When the subscriber sees a published
    // event carrying this id in `Nats-Msg-Id`, try_claim must return
    // AlreadyDone and the handler must not run.
    let msg_id = MessageId::new();
    store
        .try_claim(&msg_id, Duration::from_secs(60))
        .await
        .unwrap();
    store.mark_done(&msg_id).await.unwrap();

    let counter = Arc::new(AtomicU32::new(0));
    let options = SubscribeOptions {
        durable: "already-done-test".into(),
        filter: "events.dlq.>".into(),
        max_deliver: 3,
        ack_wait: Duration::from_secs(2),
        backoff: vec![Duration::from_millis(100)],
        ..Default::default()
    };

    let _handle = subscribe::<DlqTestEvent, _, _>(
        client.clone(),
        options,
        Arc::new(CountingHandler {
            counter: counter.clone(),
        }),
        store.clone(),
    )
    .await
    .unwrap();

    let event = DlqTestEvent {
        id: msg_id.clone(),
        value: 1,
    };
    publisher.publish(&event).await.unwrap();

    tokio::time::sleep(Duration::from_secs(2)).await;

    assert_eq!(
        counter.load(Ordering::SeqCst),
        0,
        "handler must not run when try_claim returns AlreadyDone",
    );
}
```

Note on the bug being fixed: this scenario is the same shape as a real-world redelivery after `mark_done` succeeds but `double_ack` does not land (network blip → JetStream `ack_wait` expires → same source-stream message is redelivered). With the old `delivered > 1` heuristic, that redelivery re-invoked the handler. With `try_claim`, the `AlreadyDone` outcome correctly classifies it as duplicate. The test reproduces the *state* of that scenario (key is `done`, message arrives) without depending on ack-wait timing.

The publisher serialises the event with `id: msg_id` in the JSON payload. The subscriber's `Nats-Msg-Id` header is derived from the event's id (via `Event::message_id` — verify in `bus-macros` if the macro generates this; otherwise the subscriber falls back to fresh UUIDv7 and the test fails differently, indicating macro/header wiring needs adjustment first).

- [ ] **Step 2: Run test to verify it fails**

```
cd /Users/olivier/Documents/1hoodlabs/eventbus-rs && cargo test -p bus-nats --test dlq_test duplicate_redelivery_after_done_does_not_invoke_handler
```

Expected: FAIL — compilation error first (`subscriber.rs` still references `try_insert`); after Step 3 lands, the test should fail because the handler runs more than once. After Step 4, it passes.

- [ ] **Step 3: Update subscriber to use `try_claim`**

In `crates/bus-nats/src/subscriber.rs`, update the imports near the top of the file. Replace the `bus_core::{ … idempotency::IdempotencyStore }` block with:

```rust
use bus_core::{
    error::{BusError, HandlerError},
    event::Event,
    handler::{EventHandler, HandlerCtx},
    id::MessageId,
    idempotency::{ClaimOutcome, IdempotencyStore},
};
```

Replace the entire `match store.try_insert(…)` block (currently lines 187-210) with:

```rust
match store
    .try_claim(&msg_id, processing_options.idempotency_ttl)
    .await
{
    Ok(ClaimOutcome::Claimed) => {}
    Ok(ClaimOutcome::AlreadyPending) => {
        tracing::debug!(
            %msg_id,
            delivered = info.delivered,
            "claim is pending — retrying handler",
        );
    }
    Ok(ClaimOutcome::AlreadyDone) => {
        tracing::debug!(%msg_id, "already processed — acking duplicate");
        let _ = ack::double_ack(&msg).await;
        return;
    }
    Err(error) => {
        tracing::warn!(%msg_id, "idempotency store error: {} — NAKing", error);
        let _ = ack::nak_with_delay(&msg, Duration::from_secs(1)).await;
        return;
    }
}
```

Note: `BusError` is unused in `subscriber.rs` after this change — verify with `cargo check` and remove the import if so.

- [ ] **Step 4: Run all tests**

```
cd /Users/olivier/Documents/1hoodlabs/eventbus-rs && cargo test -p bus-nats
```

Expected: all green, including:
- `duplicate_redelivery_after_done_does_not_invoke_handler` (new)
- All existing DLQ tests (Permanent/Transient/Poison paths still work because `Claimed` falls through to handler)
- `subscriber_shutdown_test` from Task 4

- [ ] **Step 5: Commit**

```
git add crates/bus-nats/src/subscriber.rs crates/bus-nats/tests/dlq_test.rs
git commit -m "fix(bus-nats): use ClaimOutcome dispatch to prevent handler double-execution"
```

---

## Task 9: Workspace-wide verification

**Files:** none

- [ ] **Step 1: Full workspace test**

```
cd /Users/olivier/Documents/1hoodlabs/eventbus-rs && cargo test --workspace
```

Expected: all crates green.

- [ ] **Step 2: Clippy check**

```
cd /Users/olivier/Documents/1hoodlabs/eventbus-rs && cargo clippy --workspace --all-targets -- -D warnings
```

Expected: no warnings.

- [ ] **Step 3: If anything fails**

Stop. Investigate the failure, document it inline (which task introduced the regression), and fix in a new follow-up commit on the same branch — do not amend prior commits.

---

## Self-Review Notes

- **Coverage:** Each of the six review issues maps to exactly one task. Issue 1 (timeout) → Task 1; Issue 2 (idempotency double-exec) → Tasks 5, 6, 7, 8; Issue 3 (TTL) → Task 2; Issue 4 (worker leak) → Task 4; Issue 5 (test flake) → Task 1 Step 4; Issue 6 (`unwrap`) → Task 3.
- **Breaking change isolation:** Task 5 introduces a breaking trait change. Tasks 6 and 7 must land in the same PR/branch as Task 5 to keep the workspace compiling. Task 8 closes the gap inside `bus-nats`.
- **Test coverage:** Each behavior change has a failing test before the implementation. Pure config refactors (Task 2) rely on existing coverage.
- **Ordering:** Tasks 1-4 are independent of the trait change and can land first as small PRs. Tasks 5-8 form one logical unit and should land together.

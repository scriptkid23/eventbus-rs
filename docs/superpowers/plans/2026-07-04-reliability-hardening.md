# Reliability Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the effectively-once violations, add subscriber reconnect and graceful drain, wire `ConnectOptions` through the builder, and remove vestigial API surface — making eventbus-rs trustworthy in production.

**Architecture:** All hot-path fixes land in `crates/bus-nats/src/subscriber.rs` (claim handling, stable fallback IDs, ack error logging, reconnect loop, drain). The facade (`crates/event-bus`) gains `connect_options` and a draining `SubscriptionHandle`. A final cleanup pass removes dead API (`buffered`, `Outbox`, `with_otel`, per-call TTL) and bumps to 0.2.0.

**Tech Stack:** Rust (edition 2024), tokio, async-nats 0.46 (JetStream), testcontainers 0.23 (integration tests need Docker running), uuid v5+v7.

**Background you need:**
- The bus consumes from NATS JetStream with a durable pull consumer. Delivery is at-least-once; an `IdempotencyStore` (NATS KV or Redis) is claimed per message to get "effectively-once" handler execution.
- `try_claim` returns `Claimed` (we own it), `AlreadyPending` (another worker may be running the handler right now), or `AlreadyDone` (processed before).
- Ack vocabulary: `double_ack` = ack and wait for server confirmation (message never redelivered); `nak_with_delay` = negative-ack, server redelivers after the delay; `term` = never redeliver.
- Integration tests spin up real NATS in Docker via testcontainers. Run them with `cargo test -p bus-nats --test <name>`. They take ~10-30s each because of container startup.
- Workspace layout: `crates/bus-core` (traits, no NATS deps), `crates/bus-nats` (transport), `crates/bus-macros` (derive), `crates/event-bus` (facade, published as `eventbus-nats`).

**Conventions:**
- All comments in English.
- Before each commit: `cargo fmt --all` and `cargo clippy --workspace --all-features -- -D warnings` must be clean.
- Commit message style in this repo: conventional commits (`fix:`, `feat:`, `refactor:`, `chore:`).

---

## File Structure

| File | Change |
|---|---|
| `crates/bus-nats/src/subscriber.rs` | Tasks 1–5: claim handling, fallback ID, ack logging, reconnect, drain |
| `crates/bus-nats/tests/subscriber_pending_test.rs` | Create (Task 1) |
| `crates/bus-nats/tests/subscriber_reconnect_test.rs` | Create (Task 4) |
| `crates/bus-nats/tests/subscriber_shutdown_test.rs` | Extend (Task 5) |
| `crates/event-bus/src/builder.rs` | Task 6: `connect_options` |
| `crates/event-bus/src/bus.rs` | Task 5: draining handle; Task 6 |
| `crates/event-bus/tests/builder_test.rs` | Extend (Task 6) |
| `crates/bus-core/src/idempotency.rs` | Task 7: drop per-call `ttl` param |
| `crates/bus-nats/src/inbox/nats_kv.rs`, `inbox/redis.rs` | Task 7 |
| `crates/bus-core/src/error.rs`, `publisher.rs` | Task 8: remove dead variants/fields |
| `Cargo.toml`, `docker-compose.yml`, `src/main.rs`, `CHANGELOG.md` | Tasks 8–9 |

---

### Task 1: `AlreadyPending` must NAK, not run the handler

**Why:** Today when `try_claim` returns `AlreadyPending`, `process_message` logs and falls through to run the handler anyway (`subscriber.rs:242-248`). With `concurrency > 1` or an early redelivery, two workers can execute the handler for the same message concurrently — breaking the effectively-once guarantee. The safe behavior is: NAK with a short delay and let a later redelivery find the claim either released (retry) or done (skip).

**Files:**
- Modify: `crates/bus-nats/src/subscriber.rs:242-248`
- Test: `crates/bus-nats/tests/subscriber_pending_test.rs` (create)

- [ ] **Step 1: Write the failing integration test**

Create `crates/bus-nats/tests/subscriber_pending_test.rs`. It uses a fake store that always reports `AlreadyPending`, so if the subscriber ever runs the handler, the counter increments and the test fails.

```rust
use async_trait::async_trait;
use bus_core::{
    error::{BusError, HandlerError},
    id::MessageId,
    idempotency::{ClaimOutcome, IdempotencyStore},
    EventHandler, HandlerCtx, Publisher,
};
use bus_nats::{subscriber::subscribe, NatsClient, NatsPublisher, StreamConfig, SubscribeOptions};
use eventbus_macros::Event;
use serde::{Deserialize, Serialize};
use std::{
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
    time::Duration,
};
use testcontainers::{
    core::{IntoContainerPort, WaitFor},
    runners::AsyncRunner,
    GenericImage, ImageExt,
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

#[derive(Debug, Serialize, Deserialize, Event)]
#[event(subject = "events.pending.created")]
struct PendingEvent {
    id: MessageId,
}

struct CountingHandler(Arc<AtomicU32>);

#[async_trait]
impl EventHandler<PendingEvent> for CountingHandler {
    async fn handle(&self, _ctx: HandlerCtx, _evt: PendingEvent) -> Result<(), HandlerError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// Store that claims to always have a pending claim held elsewhere.
struct AlwaysPendingStore;

#[async_trait]
impl IdempotencyStore for AlwaysPendingStore {
    async fn try_claim(&self, _key: &MessageId, _ttl: Duration) -> Result<ClaimOutcome, BusError> {
        Ok(ClaimOutcome::AlreadyPending)
    }
    async fn mark_done(&self, _key: &MessageId) -> Result<(), BusError> {
        Ok(())
    }
    async fn release(&self, _key: &MessageId) -> Result<(), BusError> {
        Ok(())
    }
}

#[tokio::test]
async fn already_pending_never_runs_handler() {
    let (_c, url) = start_nats().await;
    let cfg = StreamConfig {
        num_replicas: 1,
        ..Default::default()
    };
    let client = NatsClient::connect(&url, &cfg).await.unwrap();
    let publisher = NatsPublisher::new(client.clone());

    let counter = Arc::new(AtomicU32::new(0));
    let handler = Arc::new(CountingHandler(counter.clone()));

    let opts = SubscribeOptions {
        durable: "pending-worker".into(),
        filter: "events.pending.>".into(),
        ..Default::default()
    };

    let _handle = subscribe::<PendingEvent, _, _>(
        client,
        opts,
        handler,
        Arc::new(AlwaysPendingStore),
    )
    .await
    .unwrap();

    publisher
        .publish(&PendingEvent { id: MessageId::new() })
        .await
        .unwrap();

    // Give the subscriber time to receive and (wrongly) process the message.
    tokio::time::sleep(Duration::from_secs(2)).await;

    assert_eq!(
        counter.load(Ordering::SeqCst),
        0,
        "handler must not run while a claim is pending elsewhere"
    );
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p bus-nats --test subscriber_pending_test -- --nocapture`
Expected: FAIL with `handler must not run while a claim is pending elsewhere` (counter is 1 because current code falls through to the handler).

- [ ] **Step 3: Change the `AlreadyPending` arm to NAK and return**

In `crates/bus-nats/src/subscriber.rs`, replace the `AlreadyPending` match arm (currently lines 242-248):

```rust
        Ok(ClaimOutcome::AlreadyPending) => {
            // Another worker may be executing the handler for this message
            // right now. Running it here would break effectively-once, so
            // NAK and let a later redelivery observe the final claim state
            // (released -> retry, done -> skip).
            tracing::debug!(
                %msg_id,
                delivered = info.delivered,
                "claim is pending elsewhere — NAKing for later redelivery",
            );
            let _ = ack::nak_with_delay(&msg, Duration::from_secs(1)).await;
            return;
        }
```

(The `let _ =` here gets proper logging in Task 3 — do not gold-plate it now.)

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p bus-nats --test subscriber_pending_test -- --nocapture`
Expected: PASS

- [ ] **Step 5: Run the existing subscriber test to check for regressions**

Run: `cargo test -p bus-nats --test subscriber_test -- --nocapture`
Expected: PASS (`duplicate_event_handled_once` still holds — the happy path claims successfully and is unaffected).

- [ ] **Step 6: Commit**

```bash
cargo fmt --all && cargo clippy -p bus-nats -- -D warnings
git add crates/bus-nats/src/subscriber.rs crates/bus-nats/tests/subscriber_pending_test.rs
git commit -m "fix: NAK instead of running handler when idempotency claim is pending"
```

---

### Task 2: Stable fallback message ID for messages without `Nats-Msg-Id`

**Why:** When a message lacks the `Nats-Msg-Id` header (external publisher), `subscriber.rs:234` mints a *random* UUIDv7 — a different ID on every redelivery, which makes the idempotency store useless for those messages. Derive a deterministic UUIDv5 from `stream:durable:stream_sequence` instead, so every redelivery of the same message maps to the same key.

**Files:**
- Modify: `crates/bus-nats/src/subscriber.rs` (ID extraction + new helper + unit tests)
- Modify: `Cargo.toml` (workspace `uuid` gains the `v5` feature)

- [ ] **Step 1: Add the `v5` feature to uuid**

In the root `Cargo.toml`, change:

```toml
uuid         = { version = "1", features = ["v7", "serde"] }
```

to:

```toml
uuid         = { version = "1", features = ["v5", "v7", "serde"] }
```

- [ ] **Step 2: Write failing unit tests for the helper**

In `crates/bus-nats/src/subscriber.rs`, inside the existing `mod tests` at the bottom of the file, add:

```rust
    use super::fallback_message_id;

    #[test]
    fn fallback_message_id_is_deterministic() {
        let a = fallback_message_id("EVENTS", "worker-1", 42);
        let b = fallback_message_id("EVENTS", "worker-1", 42);
        assert_eq!(a, b, "same stream/durable/sequence must yield same id");
    }

    #[test]
    fn fallback_message_id_differs_per_sequence() {
        let a = fallback_message_id("EVENTS", "worker-1", 42);
        let b = fallback_message_id("EVENTS", "worker-1", 43);
        assert_ne!(a, b);
    }
```

Note: `MessageId` derives `PartialEq` in `crates/bus-core/src/id.rs` — if `assert_eq!` fails to compile because it doesn't, compare `a.to_string()` with `b.to_string()` instead.

- [ ] **Step 3: Run tests to verify they fail to compile**

Run: `cargo test -p bus-nats --lib`
Expected: FAIL — `fallback_message_id` not found.

- [ ] **Step 4: Implement the helper and use it**

Add near `compute_backoff` in `crates/bus-nats/src/subscriber.rs`:

```rust
/// Deterministic fallback ID for messages published without a `Nats-Msg-Id`
/// header. UUIDv5 over stream/durable/sequence is stable across redeliveries,
/// so the idempotency store still deduplicates such messages.
fn fallback_message_id(stream: &str, durable: &str, stream_sequence: u64) -> MessageId {
    let name = format!("{stream}:{durable}:{stream_sequence}");
    MessageId::from_uuid(uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, name.as_bytes()))
}
```

Then replace the extraction in `process_message` (currently ending at line 234 with `unwrap_or_else(|| MessageId::from_uuid(uuid::Uuid::now_v7()))`):

```rust
    let msg_id = msg
        .headers
        .as_ref()
        .and_then(|h| h.get(async_nats::header::NATS_MESSAGE_ID))
        .and_then(|v| MessageId::from_str(v.as_str()).ok())
        .unwrap_or_else(|| {
            let fallback = fallback_message_id(
                &processing_options.source,
                &processing_options.durable,
                info.stream_sequence,
            );
            tracing::warn!(
                msg_id = %fallback,
                subject = %msg.subject,
                "message has no Nats-Msg-Id header — using sequence-derived fallback id",
            );
            fallback
        });
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p bus-nats --lib`
Expected: PASS (both new tests plus the four existing `compute_backoff` tests).

- [ ] **Step 6: Commit**

```bash
cargo fmt --all && cargo clippy -p bus-nats -- -D warnings
git add Cargo.toml Cargo.lock crates/bus-nats/src/subscriber.rs
git commit -m "fix: derive stable fallback message id from stream sequence"
```

---

### Task 3: Stop swallowing ack and mark_done errors

**Why:** The hot path drops errors with `let _ = ack::double_ack(...)` etc. If `mark_done` fails but the ack succeeds, the claim is stuck `pending`; with Task 1's behavior a true duplicate would then NAK-loop to DLQ. We can't fully prevent that (distributed systems), but we must at least log every failure so operators can see it, and keep the safe ordering: `mark_done` **before** ack.

**Files:**
- Modify: `crates/bus-nats/src/subscriber.rs` (all `let _ = ack::...` sites)

- [ ] **Step 1: Add logging helpers**

Add these next to `mark_done_and_term` / `release_and_nak` in `crates/bus-nats/src/subscriber.rs`:

```rust
async fn double_ack_or_warn(msg: &Message, msg_id: &MessageId) {
    if let Err(error) = ack::double_ack(msg).await {
        tracing::warn!(%msg_id, "double ack failed — message may be redelivered: {}", error);
    }
}

async fn nak_or_warn(msg: &Message, msg_id: &MessageId, delay: Duration) {
    if let Err(error) = ack::nak_with_delay(msg, delay).await {
        tracing::warn!(%msg_id, "NAK failed — redelivery falls back to ack_wait: {}", error);
    }
}

async fn term_or_warn(msg: &Message, msg_id: &MessageId) {
    if let Err(error) = ack::term(msg).await {
        tracing::warn!(%msg_id, "TERM failed — message may be redelivered: {}", error);
    }
}
```

- [ ] **Step 2: Replace every silent drop with a helper call**

There are six sites to change in `crates/bus-nats/src/subscriber.rs` (line numbers pre-Task-1/2 edits; find by content):

1. `AlreadyPending` arm (added in Task 1): `let _ = ack::nak_with_delay(&msg, Duration::from_secs(1)).await;` → `nak_or_warn(&msg, &msg_id, Duration::from_secs(1)).await;`
2. `AlreadyDone` arm: `let _ = ack::double_ack(&msg).await;` → `double_ack_or_warn(&msg, &msg_id).await;`
3. Idempotency store `Err` arm: `let _ = ack::nak_with_delay(&msg, Duration::from_secs(1)).await;` → `nak_or_warn(&msg, &msg_id, Duration::from_secs(1)).await;`
4. Handler success arm — replace both lines:

```rust
        Ok(()) => {
            // mark_done BEFORE ack: if mark_done fails we still ack (the
            // side effects already ran once; re-running the handler would be
            // worse), but the error must be visible to operators because the
            // claim is now stuck pending until its TTL expires.
            if let Err(error) = store.mark_done(&msg_id).await {
                tracing::error!(
                    %msg_id,
                    "mark_done failed after successful handler — claim stuck pending until TTL: {}",
                    error,
                );
            }
            double_ack_or_warn(&msg, &msg_id).await;
        }
```

5. In `mark_done_and_term`: `let _ = ack::term(msg).await;` → `term_or_warn(msg, msg_id).await;`
6. In `release_and_nak`: `let _ = ack::nak_with_delay(msg, delay).await;` → `nak_or_warn(msg, msg_id, delay).await;`

Also fix the early-return in `process_message` when `msg.info()` fails — currently it returns with no ack at all. Change:

```rust
    let info = match msg.info() {
        Ok(i) => i,
        Err(e) => {
            tracing::error!("failed to get message info: {} — leaving for ack_wait redelivery", e);
            return;
        }
    };
```

(Only the log message changes — we cannot NAK meaningfully without knowing the message identity, so document the implicit ack_wait fallback.)

- [ ] **Step 3: Build and run the full bus-nats test suite**

Run: `cargo test -p bus-nats`
Expected: PASS — behavior is unchanged for the success paths; only logging was added.

- [ ] **Step 4: Commit**

```bash
cargo fmt --all && cargo clippy -p bus-nats -- -D warnings
git add crates/bus-nats/src/subscriber.rs
git commit -m "fix: log ack and mark_done failures instead of silently dropping them"
```

---

### Task 4: Subscriber reconnect with exponential backoff

**Why:** The pull loop exits permanently when `consumer.messages()` fails or the stream ends (`subscriber.rs:153-158`, `line 174`). A NATS blip kills the subscription forever with only a log line. The advisory logger (`advisory.rs:151-191`) already implements reconnect-with-backoff; apply the same pattern, and additionally re-create the consumer (it may have been deleted server-side).

**Files:**
- Modify: `crates/bus-nats/src/subscriber.rs` (restructure the spawned loop)
- Test: `crates/bus-nats/tests/subscriber_reconnect_test.rs` (create)

- [ ] **Step 1: Write the failing integration test**

Create `crates/bus-nats/tests/subscriber_reconnect_test.rs`. It deletes the durable consumer out from under the subscription (which ends the message stream), then publishes — a reconnecting subscriber recreates the consumer and still delivers:

```rust
use async_trait::async_trait;
use bus_core::{
    error::HandlerError, id::MessageId, EventHandler, HandlerCtx, Publisher,
};
use bus_nats::{
    subscriber::subscribe, NatsClient, NatsKvIdempotencyConfig, NatsKvIdempotencyStore,
    NatsPublisher, StreamConfig, SubscribeOptions,
};
use eventbus_macros::Event;
use serde::{Deserialize, Serialize};
use std::{
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
    time::Duration,
};
use testcontainers::{
    core::{IntoContainerPort, WaitFor},
    runners::AsyncRunner,
    GenericImage, ImageExt,
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

#[derive(Debug, Serialize, Deserialize, Event)]
#[event(subject = "events.reconnect.created")]
struct ReconnectEvent {
    id: MessageId,
}

struct CountingHandler(Arc<AtomicU32>);

#[async_trait]
impl EventHandler<ReconnectEvent> for CountingHandler {
    async fn handle(&self, _ctx: HandlerCtx, _evt: ReconnectEvent) -> Result<(), HandlerError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn subscriber_recovers_after_consumer_deleted() {
    let (_c, url) = start_nats().await;
    let cfg = StreamConfig {
        num_replicas: 1,
        ..Default::default()
    };
    let client = NatsClient::connect(&url, &cfg).await.unwrap();
    let publisher = NatsPublisher::new(client.clone());
    let store = Arc::new(
        NatsKvIdempotencyStore::new(
            client.jetstream().clone(),
            NatsKvIdempotencyConfig {
                num_replicas: 1,
                ..Default::default()
            },
        )
        .await
        .unwrap(),
    );

    let counter = Arc::new(AtomicU32::new(0));
    let handler = Arc::new(CountingHandler(counter.clone()));

    let opts = SubscribeOptions {
        durable: "reconnect-worker".into(),
        filter: "events.reconnect.>".into(),
        ..Default::default()
    };

    let _handle = subscribe::<ReconnectEvent, _, _>(client.clone(), opts, handler, store)
        .await
        .unwrap();

    // Sanity: baseline delivery works.
    publisher
        .publish(&ReconnectEvent { id: MessageId::new() })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(counter.load(Ordering::SeqCst), 1);

    // Delete the durable consumer server-side — the message stream ends.
    let stream = client.jetstream().get_stream("EVENTS").await.unwrap();
    stream.delete_consumer("reconnect-worker").await.unwrap();

    // Give the subscriber time to notice and reconnect (backoff starts at 200ms).
    tokio::time::sleep(Duration::from_secs(3)).await;

    publisher
        .publish(&ReconnectEvent { id: MessageId::new() })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(3)).await;

    assert_eq!(
        counter.load(Ordering::SeqCst),
        2,
        "subscriber must recreate the consumer and keep delivering"
    );
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p bus-nats --test subscriber_reconnect_test -- --nocapture`
Expected: FAIL on the final assertion (counter stays 1 — the loop exited when the consumer vanished).

- [ ] **Step 3: Restructure the spawned loop to reconnect**

In `crates/bus-nats/src/subscriber.rs`, first add reconnect constants near the top:

```rust
const RECONNECT_BACKOFF_INITIAL: Duration = Duration::from_millis(200);
const RECONNECT_BACKOFF_MAX: Duration = Duration::from_secs(30);
```

Then replace the whole `let handle = tokio::spawn(async move { ... });` block (from `let handle = tokio::spawn` down to the matching `});`, currently lines 152-201) with a version that (a) keeps consumer acquisition inside the loop so a deleted consumer is recreated, and (b) backs off between attempts. The initial `get_stream`/`get_or_create_consumer` before the spawn stays as-is for fail-fast, but capture the pieces needed to recreate it:

```rust
    let js = client.js.clone();
    let stream_name = opts.stream.clone();
    let durable = opts.durable.clone();
    let filter = opts.filter.clone();
    let max_deliver = opts.max_deliver;
    let ack_wait = opts.ack_wait;
    let consumer_backoff = opts.backoff.clone();

    let handle = tokio::spawn(async move {
        let mut workers: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();
        let mut current_consumer = Some(consumer);
        let mut reconnect_backoff = RECONNECT_BACKOFF_INITIAL;

        'reconnect: loop {
            // (Re)acquire the consumer. On the first iteration reuse the one
            // created before spawn; afterwards recreate it, because the stream
            // ending usually means the consumer or connection went away.
            let consumer = match current_consumer.take() {
                Some(c) => c,
                None => {
                    let acquired = async {
                        let stream = js
                            .get_stream(&stream_name)
                            .await
                            .map_err(|e| e.to_string())?;
                        stream
                            .get_or_create_consumer(
                                &durable,
                                build_pull_config(
                                    &durable,
                                    &filter,
                                    max_deliver,
                                    ack_wait,
                                    &consumer_backoff,
                                ),
                            )
                            .await
                            .map_err(|e| e.to_string())
                    }
                    .await;
                    match acquired {
                        Ok(c) => c,
                        Err(error) => {
                            tracing::error!(
                                stream = %stream_name,
                                durable = %durable,
                                "failed to reacquire consumer: {} — retrying after {:?}",
                                error,
                                reconnect_backoff,
                            );
                            tokio::time::sleep(reconnect_backoff).await;
                            reconnect_backoff =
                                (reconnect_backoff * 2).min(RECONNECT_BACKOFF_MAX);
                            continue 'reconnect;
                        }
                    }
                }
            };

            let mut messages = match consumer.messages().await {
                Ok(stream) => stream,
                Err(error) => {
                    tracing::error!(
                        "failed to get message stream: {} — retrying after {:?}",
                        error,
                        reconnect_backoff,
                    );
                    tokio::time::sleep(reconnect_backoff).await;
                    reconnect_backoff = (reconnect_backoff * 2).min(RECONNECT_BACKOFF_MAX);
                    continue 'reconnect;
                }
            };
            reconnect_backoff = RECONNECT_BACKOFF_INITIAL;

            loop {
                tokio::select! {
                    biased;
                    Some(joined) = workers.join_next(), if !workers.is_empty() => {
                        if let Err(error) = joined
                            && !error.is_cancelled()
                        {
                            tracing::warn!("worker task error: {}", error);
                        }
                    }
                    next = messages.next() => {
                        let Some(item) = next else {
                            tracing::warn!(
                                stream = %stream_name,
                                durable = %durable,
                                "message stream ended — reconnecting after {:?}",
                                reconnect_backoff,
                            );
                            tokio::time::sleep(reconnect_backoff).await;
                            reconnect_backoff =
                                (reconnect_backoff * 2).min(RECONNECT_BACKOFF_MAX);
                            continue 'reconnect;
                        };
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
        }
    });
```

Note the loop no longer terminates on its own — the previous `break` + `workers.shutdown().await` tail is gone. Shutdown comes back properly in Task 5 (until then, `Drop`'s `abort()` on `SubscriptionHandle` still stops everything, same as today). If clippy complains about unreachable code after the `'reconnect` loop, remove any trailing statements.

- [ ] **Step 4: Run the new test to verify it passes**

Run: `cargo test -p bus-nats --test subscriber_reconnect_test -- --nocapture`
Expected: PASS

- [ ] **Step 5: Run all bus-nats integration tests**

Run: `cargo test -p bus-nats`
Expected: PASS — in particular `subscriber_shutdown_test` (Drop still aborts) and `dlq_test`.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all && cargo clippy -p bus-nats -- -D warnings
git add crates/bus-nats/src/subscriber.rs crates/bus-nats/tests/subscriber_reconnect_test.rs
git commit -m "feat: reconnect subscriber with exponential backoff after stream failure"
```

---

### Task 5: Graceful drain on `SubscriptionHandle` and honest `EventBus::shutdown`

**Why:** Dropping `SubscriptionHandle` aborts in-flight handlers mid-execution; un-acked messages then wait out `ack_wait`. `EventBus::shutdown()` is a documented no-op. Add `drain(timeout)`: stop pulling new messages, wait for in-flight workers, then return. Keep `Drop` = abort as the escape hatch.

**Files:**
- Modify: `crates/bus-nats/src/subscriber.rs` (`SubscriptionHandle`, shutdown signal in the loop)
- Modify: `crates/event-bus/src/bus.rs` (facade handle + `shutdown` docs)
- Test: `crates/bus-nats/tests/subscriber_shutdown_test.rs` (extend)

- [ ] **Step 1: Write the failing test**

Read `crates/bus-nats/tests/subscriber_shutdown_test.rs` first to reuse its `start_nats` helper and event type. Append this test (adapting the event/handler names to what exists in that file — it has a handler that sleeps; if not, use this self-contained pair):

```rust
struct SlowHandler {
    started: Arc<AtomicU32>,
    finished: Arc<AtomicU32>,
}

#[async_trait]
impl EventHandler<ShutdownEvent> for SlowHandler {
    async fn handle(&self, _ctx: HandlerCtx, _evt: ShutdownEvent) -> Result<(), HandlerError> {
        self.started.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(500)).await;
        self.finished.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn drain_waits_for_in_flight_handler() {
    let (_c, url) = start_nats().await;
    let cfg = StreamConfig {
        num_replicas: 1,
        ..Default::default()
    };
    let client = NatsClient::connect(&url, &cfg).await.unwrap();
    let publisher = NatsPublisher::new(client.clone());
    let store = Arc::new(
        NatsKvIdempotencyStore::new(
            client.jetstream().clone(),
            NatsKvIdempotencyConfig {
                num_replicas: 1,
                ..Default::default()
            },
        )
        .await
        .unwrap(),
    );

    let started = Arc::new(AtomicU32::new(0));
    let finished = Arc::new(AtomicU32::new(0));
    let handler = Arc::new(SlowHandler {
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
        .publish(&ShutdownEvent { id: MessageId::new() })
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
```

(`ShutdownEvent` should match the derive-Event struct already in that test file; if the file names it differently, reuse the existing struct instead of adding a new one.)

- [ ] **Step 2: Run the test to verify it fails to compile**

Run: `cargo test -p bus-nats --test subscriber_shutdown_test`
Expected: FAIL — no method `drain` on `SubscriptionHandle`.

- [ ] **Step 3: Implement `drain`**

In `crates/bus-nats/src/subscriber.rs`:

Replace the `SubscriptionHandle` struct and `Drop` impl:

```rust
/// Handle to a running subscription.
///
/// - [`SubscriptionHandle::drain`] performs a graceful shutdown: stop pulling
///   new messages, wait for in-flight handlers to finish (bounded by a
///   timeout), then stop.
/// - Dropping the handle without draining aborts the loop and every in-flight
///   worker immediately; un-acked messages redeliver after `ack_wait`.
pub struct SubscriptionHandle {
    handle: Option<JoinHandle<()>>,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
}

impl SubscriptionHandle {
    /// Gracefully stop the subscription. Returns `true` when every in-flight
    /// handler finished within `timeout`; on timeout the remaining workers
    /// are aborted and `false` is returned.
    pub async fn drain(mut self, timeout: Duration) -> bool {
        let _ = self.shutdown_tx.send(true);
        let Some(mut handle) = self.handle.take() else {
            return true;
        };
        match tokio::time::timeout(timeout, &mut handle).await {
            Ok(_) => true,
            Err(_) => {
                handle.abort();
                false
            }
        }
    }
}

impl Drop for SubscriptionHandle {
    fn drop(&mut self) {
        // Abort the outer message loop. When the outer task is aborted, its
        // owned `JoinSet<()>` is dropped, which aborts every spawned worker.
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}
```

In `subscribe()`, create the channel before the spawn and thread the receiver in:

```rust
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
```

Inside the spawned task (Task 4's structure), add a shutdown arm as the first branch of the inner `tokio::select!`:

```rust
                    _ = shutdown_rx.changed() => {
                        tracing::info!(
                            stream = %stream_name,
                            durable = %durable,
                            "subscription draining — waiting for in-flight workers",
                        );
                        break 'reconnect;
                    }
```

Also check for shutdown in the reconnect-backoff sleeps so a draining subscriber does not sit in a 30s sleep. Replace each `tokio::time::sleep(reconnect_backoff).await;` inside the `'reconnect` loop with:

```rust
                    tokio::select! {
                        _ = shutdown_rx.changed() => break 'reconnect,
                        _ = tokio::time::sleep(reconnect_backoff) => {}
                    }
```

After the `'reconnect` loop (now reachable via `break 'reconnect`), drain workers:

```rust
        while workers.join_next().await.is_some() {}
```

And return the new handle shape at the end of `subscribe()`:

```rust
    Ok(SubscriptionHandle {
        handle: Some(handle),
        shutdown_tx,
    })
```

- [ ] **Step 4: Run shutdown tests**

Run: `cargo test -p bus-nats --test subscriber_shutdown_test -- --nocapture`
Expected: PASS — both the pre-existing abort-on-drop test and the new drain test.

- [ ] **Step 5: Expose drain through the facade**

In `crates/event-bus/src/bus.rs`, replace the facade `SubscriptionHandle` and `shutdown`:

```rust
/// Handle to a running subscription.
/// Call [`SubscriptionHandle::drain`] for graceful shutdown; dropping the
/// handle aborts in-flight handlers immediately.
pub struct SubscriptionHandle(bus_nats::SubscriptionHandle);

impl SubscriptionHandle {
    /// Gracefully stop this subscription. See [`bus_nats::SubscriptionHandle::drain`].
    pub async fn drain(self, timeout: std::time::Duration) -> bool {
        self.0.drain(timeout).await
    }
}
```

(The `#[allow(dead_code)]` on the tuple field goes away since `drain` now uses it.)

And make `shutdown` honest:

```rust
    /// Close the bus. Drain your `SubscriptionHandle`s first — this method
    /// only drops the underlying NATS client, which flushes on drop; it does
    /// not wait for in-flight handlers.
    pub async fn shutdown(self) -> Result<(), BusError> {
        Ok(())
    }
```

- [ ] **Step 6: Run workspace tests and commit**

Run: `cargo test --workspace`
Expected: PASS

```bash
cargo fmt --all && cargo clippy --workspace --all-features -- -D warnings
git add crates/bus-nats/src/subscriber.rs crates/bus-nats/tests/subscriber_shutdown_test.rs crates/event-bus/src/bus.rs
git commit -m "feat: graceful drain for subscriptions via SubscriptionHandle::drain"
```

---

### Task 6: `EventBusBuilder::connect_options` for auth/TLS

**Why:** `NatsClient::connect_with_options` exists, but the builder only calls `NatsClient::connect` with defaults (`builder.rs:78`), so production auth (token, NKey, TLS) forces users to bypass the facade — and examples end up opening two NATS connections. Note `async_nats::ConnectOptions` is not `Clone`; store it in an `Option` and `take` it in `build()` (which consumes `self`, so that's fine).

**Files:**
- Modify: `crates/event-bus/src/builder.rs`
- Test: `crates/event-bus/tests/builder_test.rs` (extend)

- [ ] **Step 1: Write the failing test**

Read `crates/event-bus/tests/builder_test.rs` first and reuse its NATS-container helper. Append (adjust helper names to match the file):

```rust
#[tokio::test]
async fn builder_passes_connect_options_for_auth() {
    // NATS requiring token auth: default connect must fail, token connect must succeed.
    let c = GenericImage::new("nats", "2.10-alpine")
        .with_exposed_port(4222.tcp())
        .with_wait_for(WaitFor::message_on_stderr("Server is ready"))
        .with_cmd(["-js", "--auth", "s3cr3t"])
        .start()
        .await
        .unwrap();
    let host = c.get_host().await.unwrap();
    let port = c.get_host_port_ipv4(4222).await.unwrap();
    let url = format!("nats://{}:{}", host, port);

    let store = NoopStore; // see below

    let failed = EventBusBuilder::new()
        .url(&url)
        .idempotency(NoopStore)
        .build()
        .await;
    assert!(failed.is_err(), "connect without token must fail");

    let bus = EventBusBuilder::new()
        .url(&url)
        .connect_options(eventbus_nats::ConnectOptions::with_token("s3cr3t".into()))
        .idempotency(store)
        .build()
        .await;
    assert!(bus.is_ok(), "connect with token must succeed");
}
```

Add a no-op store at the top of the test file (the builder requires one):

```rust
struct NoopStore;

#[async_trait::async_trait]
impl bus_core::idempotency::IdempotencyStore for NoopStore {
    async fn try_claim(
        &self,
        _key: &bus_core::id::MessageId,
        _ttl: std::time::Duration,
    ) -> Result<bus_core::idempotency::ClaimOutcome, bus_core::error::BusError> {
        Ok(bus_core::idempotency::ClaimOutcome::Claimed)
    }
    async fn mark_done(
        &self,
        _key: &bus_core::id::MessageId,
    ) -> Result<(), bus_core::error::BusError> {
        Ok(())
    }
    async fn release(
        &self,
        _key: &bus_core::id::MessageId,
    ) -> Result<(), bus_core::error::BusError> {
        Ok(())
    }
}
```

Check `crates/event-bus/src/lib.rs` re-exports `ConnectOptions` (bus-nats re-exports it at `bus_nats::ConnectOptions`); if the facade doesn't re-export it yet, add `pub use bus_nats::ConnectOptions;` to `crates/event-bus/src/lib.rs` as part of Step 3.

- [ ] **Step 2: Run the test to verify it fails to compile**

Run: `cargo test -p eventbus-nats --test builder_test`
Expected: FAIL — no method `connect_options` on `EventBusBuilder`.

- [ ] **Step 3: Implement**

In `crates/event-bus/src/builder.rs`:

Add the field to the struct and its initializer:

```rust
pub struct EventBusBuilder {
    url: Option<String>,
    stream_cfg: StreamConfig,
    idempotency: Option<Arc<dyn IdempotencyStore>>,
    connect_options: Option<bus_nats::ConnectOptions>,
    dlq: Option<DlqConfig>,
}
```

(`_otel` is removed in Task 8; if executing tasks in order it is still present here — keep it until Task 8.)

In `new()`: `connect_options: None,`

Add the setter:

```rust
    /// Custom NATS connection options (auth token, NKey, TLS, cluster URLs...).
    /// Defaults to `ConnectOptions::default()` when not set.
    pub fn connect_options(mut self, options: bus_nats::ConnectOptions) -> Self {
        self.connect_options = Some(options);
        self
    }
```

And use it in `build()`, replacing `let client = NatsClient::connect(&url, &self.stream_cfg).await?;`:

```rust
        let client = match self.connect_options {
            Some(options) => {
                NatsClient::connect_with_options(&url, options, &self.stream_cfg).await?
            }
            None => NatsClient::connect(&url, &self.stream_cfg).await?,
        };
```

If the facade lib doesn't re-export `ConnectOptions`, add to `crates/event-bus/src/lib.rs`:

```rust
pub use bus_nats::ConnectOptions;
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p eventbus-nats --test builder_test -- --nocapture`
Expected: PASS (all builder tests).

- [ ] **Step 5: Commit**

```bash
cargo fmt --all && cargo clippy -p eventbus-nats -- -D warnings
git add crates/event-bus/src/builder.rs crates/event-bus/src/lib.rs crates/event-bus/tests/builder_test.rs
git commit -m "feat: accept custom ConnectOptions in EventBusBuilder for auth/TLS"
```

---

### Task 7: Remove the ignored per-call idempotency TTL

**Why:** `IdempotencyStore::try_claim` takes a `ttl: Duration` that **both** backends ignore (NATS KV uses bucket-level `max_age`, Redis uses config-level `ttl` — see `inbox/nats_kv.rs:71` and `inbox/redis.rs:77`). `SubscribeOptions.idempotency_ttl` therefore silently does nothing. Remove the parameter and the option so the API stops lying. This is a breaking change — acceptable pre-1.0, recorded in CHANGELOG in Task 9.

**Files:**
- Modify: `crates/bus-core/src/idempotency.rs`
- Modify: `crates/bus-nats/src/inbox/nats_kv.rs`, `crates/bus-nats/src/inbox/redis.rs`
- Modify: `crates/bus-nats/src/subscriber.rs` (`SubscribeOptions`, `ProcessingOptions`, call site)
- Modify: any tests/examples implementing `IdempotencyStore` or setting `idempotency_ttl` (find them in Step 3)

- [ ] **Step 1: Change the trait**

In `crates/bus-core/src/idempotency.rs`, change the signature (and remove the now-unused `use std::time::Duration;`):

```rust
    /// State-aware claim. Atomically inserts the key in `pending` state if
    /// absent, otherwise reports the existing state.
    ///
    /// Key expiry is a property of the store (e.g. bucket `max_age` for NATS
    /// KV, config `ttl` for Redis), not of individual claims.
    async fn try_claim(&self, key: &MessageId) -> Result<ClaimOutcome, BusError>;
```

- [ ] **Step 2: Update both store impls**

`crates/bus-nats/src/inbox/nats_kv.rs`: `async fn try_claim(&self, key: &MessageId) -> Result<ClaimOutcome, BusError> {` (drop `_ttl`; remove `use std::time::Duration;` if now unused — note `Duration` is still used by the config struct, so it stays). Also update the config doc comment at lines 24-26 to drop the sentence about the ignored per-call argument:

```rust
    /// Bucket-level TTL applied uniformly to every key.
    pub max_age: Duration,
```

`crates/bus-nats/src/inbox/redis.rs`: same signature change, drop `_ttl` (keep `use std::time::Duration;` — the config uses it).

- [ ] **Step 3: Update the subscriber and find remaining callers**

In `crates/bus-nats/src/subscriber.rs`:
- Delete `pub const DEFAULT_IDEMPOTENCY_TTL: Duration = ...` (line 29) and the `SECONDS_PER_DAY` const if nothing else uses it.
- Delete `idempotency_ttl` from `SubscribeOptions` (field + doc + `Default` impl) and from `ProcessingOptions` (field + construction).
- Change the call site to `store.try_claim(&msg_id).await`.

Then find every other reference:

Run: `rg -n "idempotency_ttl|DEFAULT_IDEMPOTENCY_TTL|try_claim" --type rust`

Update each: test fakes implementing the trait (e.g. `AlwaysPendingStore` from Task 1, `NoopStore` from Task 6), `crates/bus-nats/tests/inbox_kv_test.rs`, `crates/bus-nats/tests/inbox_redis_test.rs` (drop the ttl argument from direct `try_claim` calls), and `examples/03-idempotent-handler` if it sets `idempotency_ttl`.

- [ ] **Step 4: Build the whole workspace including feature-gated code**

Run: `cargo build --workspace --all-features && cargo test --workspace`
Expected: compiles clean; all tests PASS. (Redis tests need Docker; they run via testcontainers like the NATS ones. If the redis tests are feature-gated, also run `cargo test -p bus-nats --features redis-inbox`.)

- [ ] **Step 5: Commit**

```bash
cargo fmt --all && cargo clippy --workspace --all-features -- -D warnings
git add -A
git commit -m "refactor!: remove ignored per-call TTL from IdempotencyStore::try_claim"
```

---

### Task 8: Remove vestigial API surface and dead files

**Why:** Leftovers from removed features (sqlite buffer, circuit breaker) confuse the public API: `BusError::Outbox` is never constructed, `BusError::NatsUnavailable` only appears in a test, `PubReceipt.buffered` is hardcoded `false`, `with_otel()` sets a flag nobody reads, `testing.rs` is empty, root `src/main.rs` is an orphan hello-world, and docker-compose starts an unused Postgres.

**Files:**
- Modify: `crates/bus-core/src/error.rs`, `crates/bus-core/src/publisher.rs`, `crates/bus-core/tests/error_test.rs`
- Modify: `crates/bus-nats/src/publisher.rs`, `crates/bus-nats/src/lib.rs`, `crates/bus-nats/tests/publisher_test.rs`
- Modify: `crates/event-bus/src/builder.rs`
- Delete: `crates/bus-nats/src/testing.rs`, `src/main.rs`
- Modify: `docker-compose.yml`, root `Cargo.toml`

- [ ] **Step 1: Remove dead error variants**

In `crates/bus-core/src/error.rs`, delete the `Outbox` and `NatsUnavailable` variants:

```rust
/// Top-level error type for event bus operations.
#[derive(Debug, Error)]
pub enum BusError {
    #[error("nats: {0}")]
    Nats(String),

    #[error("publish: {0}")]
    Publish(String),

    #[error("idempotency: {0}")]
    Idempotency(String),

    #[error("serialization: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("handler: {0}")]
    Handler(#[from] HandlerError),
}
```

In `crates/bus-core/tests/error_test.rs`, delete the test that constructs `BusError::NatsUnavailable` (around line 27) and any `Outbox` usage.

- [ ] **Step 2: Remove `PubReceipt.buffered`**

In `crates/bus-core/src/publisher.rs`, delete the `buffered` field (lines 16-17). In `crates/bus-nats/src/publisher.rs`, delete `buffered: false,` (line 49). In `crates/bus-nats/tests/publisher_test.rs`, delete `assert!(!receipt.buffered);` (line 50).

- [ ] **Step 3: Remove `with_otel`**

In `crates/event-bus/src/builder.rs`, delete the `_otel: bool` field, its `new()` initializer, and the `with_otel()` method (lines 10, 20, 56-59 pre-Task-6 numbering). Check nothing references it:

Run: `rg -n "with_otel|_otel" --type rust`
Expected: no matches after the edit.

- [ ] **Step 4: Delete dead files and docker-compose Postgres**

```bash
git rm crates/bus-nats/src/testing.rs src/main.rs
```

Remove the module declaration from `crates/bus-nats/src/lib.rs` (lines 11-12):

```rust
#[cfg(test)]
pub(crate) mod testing;
```

In `docker-compose.yml`, delete the entire `postgres` service block (lines ~20-35 — verify by reading the file; nothing in the codebase references Postgres: confirm with `rg -ni postgres --type rust` → expect no matches).

- [ ] **Step 5: Add MSRV to the workspace**

In root `Cargo.toml` under `[workspace.package]`, add (README documents Rust 1.85+):

```toml
rust-version = "1.85"
```

Each member crate must inherit it — check every `crates/*/Cargo.toml` `[package]` section and add `rust-version.workspace = true` alongside the other `.workspace = true` inherits.

- [ ] **Step 6: Verify and commit**

Run: `cargo build --workspace --all-features && cargo test --workspace`
Expected: PASS

```bash
cargo fmt --all && cargo clippy --workspace --all-features -- -D warnings
git add -A
git commit -m "refactor!: remove vestigial API (Outbox, buffered, with_otel) and dead files"
```

---

### Task 9: Changelog, version bump, README sync

**Why:** Tasks 5–8 are breaking or behavior-visible changes. Record them and bump to 0.2.0 across the workspace. Also fix the two README/metadata inconsistencies found during audit.

**Files:**
- Modify: `CHANGELOG.md`, root `Cargo.toml`, `crates/event-bus/Cargo.toml`, `README.md`

- [ ] **Step 1: Bump versions**

In root `Cargo.toml`:
- `[workspace.package] version = "0.2.0"`
- In `[workspace.dependencies]`, update the four internal crate `version` fields to `0.2.0` (they must stay aligned per the existing comment).

In `crates/event-bus/Cargo.toml`, remove the explicit `version = "0.1.2"` override so it inherits the workspace `0.2.0` (or set it to `0.2.0` if it must stay explicit — read the file and follow whichever pattern the other crates use).

- [ ] **Step 2: Write the changelog entry**

Read `CHANGELOG.md` to match its existing format, then add a `0.2.0` section at the top with these entries:

```markdown
## 0.2.0 — 2026-07-04

### Breaking changes
- `IdempotencyStore::try_claim` no longer takes a `ttl` parameter; key expiry
  is configured on the store (`NatsKvIdempotencyConfig.max_age`,
  `RedisIdempotencyConfig.ttl`). `SubscribeOptions.idempotency_ttl` and
  `DEFAULT_IDEMPOTENCY_TTL` are removed.
- Removed dead API: `BusError::Outbox`, `BusError::NatsUnavailable`,
  `PubReceipt.buffered`, `EventBusBuilder::with_otel`.

### Fixed
- **Effectively-once under concurrency:** a message whose idempotency claim is
  `AlreadyPending` is now NAKed instead of running the handler concurrently.
- Messages published without a `Nats-Msg-Id` header get a deterministic
  fallback ID derived from stream/consumer/sequence, so redeliveries
  deduplicate correctly.
- Ack, NAK, TERM, and `mark_done` failures in the consume path are now logged
  instead of silently ignored.

### Added
- Subscriber reconnects with exponential backoff (200ms..30s) when the message
  stream ends or the consumer is deleted, instead of exiting permanently.
- `SubscriptionHandle::drain(timeout)` for graceful shutdown: stops pulling,
  waits for in-flight handlers, aborts only on timeout.
- `EventBusBuilder::connect_options(...)` to pass `async_nats::ConnectOptions`
  (token/NKey/TLS/cluster) through the facade.
- `rust-version = "1.85"` (MSRV) now enforced in Cargo metadata.
```

- [ ] **Step 3: Sync the README**

In `README.md`:
- Fix the CI badge / repository URL mismatch: `Cargo.toml` says `https://github.com/scriptkid23/eventbus-rs` while the badge points at `1hoodlabs/eventbus-rs`. Ask the user which is canonical if unclear from git remotes (`git remote -v`); make both match it.
- Update the graceful-shutdown section to recommend `handle.drain(timeout).await` before `bus.shutdown()`.
- Remove or update any mention of `idempotency_ttl` / per-call TTL.

- [ ] **Step 4: Final verification**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-features -- -D warnings && cargo test --workspace`
Expected: all clean, all PASS.

- [ ] **Step 5: Commit**

```bash
git add CHANGELOG.md Cargo.toml Cargo.lock crates/event-bus/Cargo.toml README.md
git commit -m "chore: release 0.2.0 — reliability hardening"
```

---

## Out of Scope (deliberately)

- GitHub Actions CI workflow (repo's CI lives elsewhere per README badge — coordinate with the user first).
- New examples (DLQ, Redis inbox, auth) — worth a separate docs-focused plan.
- `max_ack_pending` tuning and publish-side rate limiting — no observed need yet (YAGNI).
- OpenTelemetry integration to replace the removed `with_otel` stub — separate feature plan.

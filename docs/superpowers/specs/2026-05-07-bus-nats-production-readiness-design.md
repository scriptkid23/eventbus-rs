# `bus-nats` Production-Readiness Pass (v0.1.1) — Design Spec

**Status:** Draft for review
**Date:** 2026-05-07
**Scope:** `crates/bus-nats` only (no changes to `bus-core`, `bus-macros`, or `event-bus` aside from feature/dep cleanup transitively triggered by this work).
**Approach:** Single PR releasing `v0.1.1` with all changes batched (CHANGELOG calls out breaking changes — pre-1.0).

---

## 1. Background

`crates/bus-nats` is the NATS JetStream backend for `eventbus-rs`. A read-through of the crate revealed a mix of (a) primitives that are advertised in `README.md` but never wired into the publish/consume pipeline, (b) configuration values hardcoded in ways that are unsafe in a clustered production deployment, and (c) one outright bug in the DLQ path. This spec proposes a bounded set of changes — six items, all classified as critical — to bring the crate to a state where it can be deployed against a 3-node JetStream cluster without surprises.

Important hardening items (graceful shutdown, publish timeout, jitter, `max_ack_pending`, config reconciliation, payload size caps, metrics) are explicitly **deferred to v0.2** to keep this pass small and reviewable.

## 2. Goals & non-goals

**Goals**
- The README must accurately describe what the crate does today. No "shipped" claims for code that is a stub or that exists but is never called from any production path.
- A `RedisIdempotencyStore` that actually works as an `IdempotencyStore` impl, with atomic semantics under concurrent JetStream redelivery.
- A `NatsKvIdempotencyStore` whose bucket name and replica count are tunable per deployment.
- A connection API that can carry credentials, TLS, and cluster URLs — i.e. anything `async_nats::ConnectOptions` already supports.
- The DLQ stream is created automatically the first time a subscriber needs it, not lazily on terminal failure.

**Non-goals**
- No new transport backends.
- No metrics/Prometheus exporter (covered by `tracing` events; see README §8).
- No graceful shutdown overhaul, no jitter, no `max_ack_pending` exposure.
- No outbox/transactional publishing (out of scope per README §4 forever).
- No splitting of stream provisioning from connection (deferred to v0.2 / IaC story).

## 3. Items in scope

| ID | Item | Type |
|----|------|------|
| C1 | Implement `RedisIdempotencyStore` end-to-end | feat |
| C2 | Delete `circuit_breaker.rs` + tests + README claims | chore |
| C3 | Delete `sqlite_buffer.rs`, feature `sqlite-buffer`, dep `rusqlite` | chore |
| C4 | Auto-create DLQ stream on `subscribe()` when `DlqOptions` is set | bug |
| C5 | `NatsKvIdempotencyConfig` for bucket name + replicas + max-age | feat |
| C6 | `NatsClient::connect_with_options` taking `async_nats::ConnectOptions` | feat |

---

## 4. Design — by item

### 4.1 — Cleanup: remove unused primitives (C2, C3)

`circuit_breaker.rs` and `sqlite_buffer.rs` are self-contained, well-tested primitives that **are never called from `NatsPublisher`, `NatsClient`, or any subscriber code path**. The README markets them as production features ("Circuit breaker + SQLite fallback. When NATS is unavailable, publishes spool to disk and replay on recovery."), but no spool-and-replay code exists. Keeping them in-tree without wiring is false advertising and produces phantom maintenance work.

**YAGNI removal**: delete the modules. If a future user needs either primitive, the implementations live in git history at tag `v0.1.0` and can be re-introduced with proper wiring.

#### Files removed
- `crates/bus-nats/src/circuit_breaker.rs`
- `crates/bus-nats/src/sqlite_buffer.rs`
- `crates/bus-nats/tests/circuit_breaker_test.rs`
- `crates/bus-nats/tests/sqlite_test.rs`

#### `crates/bus-nats/src/lib.rs`
Remove:
```rust
pub mod circuit_breaker;
#[cfg(feature = "sqlite-buffer")]
pub mod sqlite_buffer;
#[cfg(feature = "sqlite-buffer")]
pub use sqlite_buffer::{BufferRow, SqliteBuffer};
```

#### `crates/bus-nats/Cargo.toml`
Remove:
- Feature `sqlite-buffer = ["dep:rusqlite"]`.
- Optional dep `rusqlite = { workspace = true, optional = true }`.
- The `[[test]] name = "sqlite_test"` block.

#### Workspace `Cargo.toml`
Verify with `rg "rusqlite" --type toml` — if no other crate uses `rusqlite`, remove it from `[workspace.dependencies]`.

#### `event-bus` umbrella crate
Verify and remove the `sqlite-buffer` feature re-export if present. Update its `Cargo.toml` and `README` accordingly.

#### `README.md` edits
1. **Strapline** (~line 5): drop "circuit breaker" — replace with "with idempotent inbox + DLQ".
2. **Features bullet list**: delete "Circuit breaker + SQLite fallback…" entirely.
3. **Cargo features table**: delete the two `sqlite-buffer` rows (event-bus and bus-nats).
4. **Implementation status table**: delete the "Circuit breaker (Closed/Open/HalfOpen)" and "SQLite fallback buffer" rows.
5. **Quick install snippet**: drop the `sqlite-buffer` feature from any sample install line.
6. **Architecture diagram (mermaid)**: verify and remove any node referencing circuit-breaker / SQLite. The current snippet does not appear to reference them, but double-check during implementation.

### 4.2 — `RedisIdempotencyStore`: full implementation (C1)

Today `inbox/redis.rs` contains exactly `pub struct RedisIdempotencyStore;` — no methods, no `IdempotencyStore` impl. This must be a working backend with the same semantics as `NatsKvIdempotencyStore`.

#### State model

Two values, mirroring NATS KV:
- `"pending"` — claim is held, handler is in-flight.
- `"done"` — handler completed; future deliveries should ack-as-duplicate.

Keys are prefixed to keep namespaces separate when multiple apps share one Redis instance.

#### TTL model — store-level, matches NATS KV

The `IdempotencyStore` trait passes `ttl` to `try_claim` but not to `mark_done`. The existing `NatsKvIdempotencyStore` ignores the per-call `ttl` and uses a bucket-level `max_age` instead. The Redis backend will follow the same convention: TTL is configured once on the store and reused for every operation. This keeps semantics consistent across backends, simplifies the implementation (no per-key TTL bookkeeping), and matches how teams typically operate idempotency stores in practice (one window for the whole service).

#### Atomic `try_claim` via Lua

`SET NX EX` alone cannot distinguish "no key yet" from "key exists with state X" in a single round-trip. A Lua script gives atomic read-and-set:

```lua
-- KEYS[1] = full key
-- ARGV[1] = ttl in milliseconds (from RedisIdempotencyConfig.ttl)
local current = redis.call('GET', KEYS[1])
if current == false then
  redis.call('SET', KEYS[1], 'pending', 'PX', ARGV[1])
  return 'claimed'
elseif current == 'done' then
  return 'already_done'
else
  return 'already_pending'
end
```

The `redis::Script` type caches the SHA1 and uses `EVALSHA` with automatic `EVAL`-fallback on `NOSCRIPT`, so no extra logic is needed in our code. The per-call `_ttl` argument from the trait is ignored (named `_ttl` to silence the lint).

#### Other operations

- `mark_done(key)` → `SET <full_key> 'done' PX <config.ttl_ms>`. Reuses the configured TTL.
- `release(key)` → `DEL <full_key>`. Releases a pending claim so the next delivery can claim it again.

#### Public types

```rust
#[derive(Debug, Clone)]
pub struct RedisIdempotencyConfig {
    /// Redis URL — `redis://host:6379` or `rediss://...` for TLS.
    pub url: String,
    /// Prefix prepended to msg_id when storing keys.
    /// Default: "eventbus:processed:".
    pub key_prefix: String,
    /// TTL applied to every claim/done entry. Mirrors `NatsKvIdempotencyConfig.max_age`.
    /// Default: 7 days.
    pub ttl: Duration,
}

impl Default for RedisIdempotencyConfig {
    fn default() -> Self {
        Self {
            url: "redis://localhost:6379".into(),
            key_prefix: "eventbus:processed:".into(),
            ttl: Duration::from_secs(7 * 24 * 3600),
        }
    }
}

pub struct RedisIdempotencyStore {
    conn: redis::aio::ConnectionManager,
    key_prefix: String,
    ttl_ms: u64,
    claim_script: redis::Script,
}

impl RedisIdempotencyStore {
    pub async fn connect(cfg: RedisIdempotencyConfig) -> Result<Self, BusError> { /* ... */ }
}
```

`redis::aio::ConnectionManager` handles automatic reconnection — no user-side retry loop required.

#### Error mapping

All `redis::RedisError` paths map to `BusError::Idempotency(format!("redis: {e}"))`. `NOSCRIPT` is handled transparently by `Script::invoke_async`.

#### Cargo dependency

```toml
# workspace Cargo.toml
redis = { version = "0.27", features = ["tokio-comp", "connection-manager", "script"] }
```

(Exact version pinned at implementation time — `0.27` is a placeholder; pick the latest at the time of the PR.)

### 4.3 — `NatsKvIdempotencyConfig` (C5)

Current code in `inbox/nats_kv.rs:24-39` hardcodes `bucket: "eventbus_processed"` and `num_replicas: 1`. This is unsafe in a multi-tenant setup (collisions on bucket name) and even for a single tenant on R3 cluster (KV state is on a single node — loss of that node loses inbox state, which means handlers re-run, which defeats the purpose of having an idempotency store).

#### New config struct

```rust
#[derive(Debug, Clone)]
pub struct NatsKvIdempotencyConfig {
    /// KV bucket name. Default: "eventbus_processed".
    pub bucket: String,
    /// Number of replicas. Default: 1 (single-node dev). For production, set to
    /// match your stream's replica count (typically 3).
    pub num_replicas: usize,
    /// Bucket-level TTL applied uniformly to every key. The `ttl` argument to
    /// `try_claim` is currently ignored (matching the existing 0.1.0 behavior).
    pub max_age: Duration,
}

impl Default for NatsKvIdempotencyConfig {
    fn default() -> Self {
        Self {
            bucket: "eventbus_processed".into(),
            num_replicas: 1,
            max_age: Duration::from_secs(7 * 24 * 3600), // 7 days
        }
    }
}
```

#### Updated `NatsKvIdempotencyStore::new`

```rust
impl NatsKvIdempotencyStore {
    pub async fn new(
        js: jetstream::Context,
        cfg: NatsKvIdempotencyConfig,
    ) -> Result<Self, BusError> {
        let store = js.create_key_value(kv::Config {
            bucket: cfg.bucket,
            history: 1,
            max_age: cfg.max_age,
            num_replicas: cfg.num_replicas,
            ..Default::default()
        })
        .await
        .map_err(|e| BusError::Idempotency(e.to_string()))?;

        Ok(Self { store })
    }
}
```

#### README §2 — sizing guidance

Add a row to the existing table:

| Setting | Dev / single-node | Production |
| --- | --- | --- |
| `NatsKvIdempotencyConfig.num_replicas` | `1` | `3` (match stream replicas) |

#### Default rationale

Default `num_replicas = 1` keeps the Quick Start runnable against a single-node NATS server (matches the StreamConfig override pattern already shown in Quick Start: users explicitly set `num_replicas: 1` for dev there too). Production deployments must override the KV config to match their cluster size — documented prominently in README §2.

### 4.4 — DLQ stream auto-create (C4)

#### Bug

`subscriber.rs::process_message` calls `publish_to_dlq()` to subject `dlq.<source>.<durable>` on terminal failure. But no code path calls `ensure_dlq_stream()` for the corresponding stream `DLQ_<source>_<durable>` before the first publish. The first time a message hits a terminal failure, the publish has no listening stream and fails. The subscriber then calls `release_and_nak`, the message is redelivered, fails again, publish-to-DLQ fails again — infinite loop until `max_deliver` exhausts and the same thing happens again, cycling forever.

#### Fix

In `subscribe()`, after `get_or_create_consumer` and before spawning the message loop, ensure the DLQ stream exists if `opts.dlq` is `Some`:

```rust
// crates/bus-nats/src/subscriber.rs (in subscribe(), after consumer creation)
if let Some(dlq_opts) = opts.dlq.as_ref() {
    let dlq_stream_name = dlq::dlq_stream_name(&opts.stream, &opts.durable);
    let dlq_subject = dlq::dlq_subject(&opts.stream, &opts.durable);
    dlq::ensure_dlq_stream(
        &client.js,
        &dlq_stream_name,
        &dlq_subject,
        &dlq_opts.config,
    )
    .await
    .map_err(|e| BusError::Nats(format!("ensure dlq stream {dlq_stream_name}: {e}")))?;
}
```

`ensure_dlq_stream` is already idempotent (`get_or_create_stream`), so this is safe to call on every subscribe and safe with respect to existing `dlq_test.rs` tests that pre-create the stream themselves.

#### Failure mode

If the NATS user lacks permission to create the DLQ stream (e.g., IaC-managed cluster), `subscribe()` returns an error. The app refuses to start — fail-fast. Better an obvious error at deploy time than a silent failure on the first poison message hours into production.

#### README naming consistency fix

README §5 currently says:
> Each subscription gets its own DLQ stream named `EVENTS_DLQ_<durable>`...

But the helper produces `DLQ_<source>_<durable>` (e.g., `DLQ_EVENTS_payments-worker`). Code is the source of truth. Update README to:
> Each subscription gets its own DLQ stream named `DLQ_<source-stream>_<durable>` (e.g. `DLQ_EVENTS_payments-worker`)...

#### README §1 — permission update

Add to the "least-privilege user" bullet list in §1:
- `pub` on `dlq.>` (for DLQ message publish)
- `pub` on `$JS.API.STREAM.CREATE.*` so the app can auto-create the DLQ stream on first subscribe (NATS subject wildcards match a single token; stream names like `DLQ_EVENTS_payments-worker` are one token, so `*` is sufficient — use `>` only if you also need wildcard rights deeper in the API tree).

If your IaC pipeline provisions DLQ streams ahead of time, the app still attempts an idempotent `get_or_create` — the call no-ops if the stream already exists.

### 4.5 — `NatsClient::connect_with_options` (C6)

Today `NatsClient::connect(url, stream_cfg)` only accepts a `&str` URL. README §1 hints "configure on `NatsClient` directly when you need credentials beyond a plain URL" but no such API exists.

#### Approach

Passthrough `async_nats::ConnectOptions`. async-nats already has a battle-tested fluent builder (creds_file, JWT, NKey, user/pass, TLS, max_reconnects, ping_interval, name, request_timeout, …). Wrapping it in our own typed config would only ever be a strict subset and would lock us into chasing async-nats releases.

#### API

```rust
impl NatsClient {
    /// Connect using simplest defaults. Equivalent to
    /// `connect_with_options(url, ConnectOptions::default(), stream_cfg)`.
    pub async fn connect(url: &str, stream_cfg: &StreamConfig) -> Result<Self, BusError> {
        Self::connect_with_options(url, async_nats::ConnectOptions::default(), stream_cfg).await
    }

    /// Connect with caller-supplied `async_nats::ConnectOptions`. Use this for
    /// auth, TLS, cluster URL lists, custom ping interval, etc.
    pub async fn connect_with_options(
        url: &str,
        options: async_nats::ConnectOptions,
        stream_cfg: &StreamConfig,
    ) -> Result<Self, BusError> {
        let client = options
            .connect(url)
            .await
            .map_err(|e| BusError::Nats(e.to_string()))?;
        let js = jetstream::new(client);
        ensure_stream(&js, stream_cfg)
            .await
            .map_err(|e| BusError::Nats(e.to_string()))?;
        Ok(Self { js })
    }
}
```

#### Re-export

`crates/bus-nats/src/lib.rs`:
```rust
pub use async_nats::ConnectOptions;
```

So application code only depends on `bus_nats` and does not need to add `async-nats` to its own `Cargo.toml`.

#### README §1 — auth examples

Replace the single hint with three concrete examples:

```rust
use bus_nats::{NatsClient, ConnectOptions, StreamConfig};
use std::time::Duration;

// 1) Credentials file (NATS user JWT)
let opts = ConnectOptions::with_credentials_file("/etc/nats/app.creds").await?;
let client = NatsClient::connect_with_options(
    "nats://nats-0:4222,nats://nats-1:4222,nats://nats-2:4222",
    opts,
    &StreamConfig::default(),
).await?;

// 2) User / password + TLS + tuning
let opts = ConnectOptions::with_user_and_password("svc-orders".into(), pass)
    .require_tls(true)
    .max_reconnects(Some(60))
    .ping_interval(Duration::from_secs(20))
    .name("orders-worker".into());
let client = NatsClient::connect_with_options(url, opts, &stream_cfg).await?;

// 3) NKey (seed)
let opts = ConnectOptions::with_nkey(seed.into());
```

`async_nats::ConnectOptions::connect` already accepts `impl ToServerAddrs`, and `&str` for `"a,b,c"` is parsed as a comma-separated server list — no extra parsing needed.

#### Deferred

Connection event callbacks (`ConnectOptions::event_callback(...)`) — useful for tracing Disconnected / Reconnected / ServerError events — are deferred to v0.2. They require either a callback signature exposed via our crate or a channel-based handle. For 0.1.1, users that need this can wrap `connect_with_options` themselves and attach their own callback.

---

## 5. Testing strategy

### Test matrix

| File | Action | Notes |
|---|---|---|
| `tests/circuit_breaker_test.rs` | **Delete** | Per §4.1. |
| `tests/sqlite_test.rs` | **Delete** | Per §4.1. |
| `tests/dlq_test.rs` | **Keep** | All current tests pre-create the DLQ stream explicitly. Subscriber's auto-create is idempotent — tests still pass unchanged. Optionally remove redundant pre-creates in a follow-up. |
| `tests/inbox_kv_test.rs` | **Update** | Migrate to `NatsKvIdempotencyConfig::default()` (which is single-node-friendly). Override `max_age` where the test asserts on TTL. |
| `tests/publisher_test.rs` | Keep unchanged | |
| `tests/subscriber_test.rs` | Keep unchanged | |
| `tests/subscriber_shutdown_test.rs` | Keep unchanged | |
| `tests/inbox_redis_test.rs` | **New** | See cases below. `required-features = ["redis-inbox"]`. |
| `tests/dlq_auto_create_test.rs` | **New** | Subscribe with `DlqOptions::default()` against a brand new NATS instance with no DLQ stream pre-created. Send one poison-payload message → assert stream `DLQ_<source>_<durable>` exists and contains the dead-letter. |
| `tests/connect_options_test.rs` | **New** | NATS testcontainer with basic-auth enabled. Assert `connect_with_options(..., with_user_and_password(...))` succeeds; assert `connect(...)` (no auth) fails. |

### Redis test cases (`tests/inbox_redis_test.rs`)

1. `try_claim` first call → `Claimed`.
2. `try_claim` second call same key → `AlreadyPending`.
3. `mark_done` then `try_claim` → `AlreadyDone`.
4. `release` then `try_claim` → `Claimed` again.
5. 50 concurrent tasks racing `try_claim` on one key → exactly one `Claimed`, the rest `AlreadyPending`.
6. TTL expiry: configure store with short TTL (1s for the test), wait 2s, `try_claim` → `Claimed`.

### Cargo deps

```toml
# crates/bus-nats/Cargo.toml [dev-dependencies]
testcontainers-modules = { workspace = true, features = ["nats", "redis"] }
```

### Cargo `[[test]]` blocks

Remove `name = "sqlite_test"`. Add:
```toml
[[test]]
name = "inbox_redis_test"
required-features = ["redis-inbox"]
```

### CI / README "Required local checks"

Replace
```bash
cargo test -p bus-nats --features sqlite-buffer
```
with
```bash
cargo test -p bus-nats --features redis-inbox
```

Update `.github/workflows/*` mirror (verify and edit during implementation).

### Acknowledged gaps

- `num_replicas: 3` only behaves correctly on a real 3-node cluster. Single-node tests just assert the field is forwarded into `kv::Config` (verifiable by reading back the bucket config). Not worth gold-plating.
- mTLS / NKey paths are not tested — async-nats already has dedicated coverage for these. We trust delegation.

---

## 6. Migration & CHANGELOG (v0.1.1)

### Breaking changes

| API | Before | After |
|---|---|---|
| `bus_nats::circuit_breaker::*` | exported | **removed** |
| `bus_nats::SqliteBuffer`, `BufferRow` | exported (feature-gated) | **removed** |
| Cargo feature `bus-nats/sqlite-buffer` | exists | **removed** |
| Cargo feature `event-bus/sqlite-buffer` | exists | **removed** |
| `NatsKvIdempotencyStore::new(js, max_age)` | `(Context, Duration)` | `(Context, NatsKvIdempotencyConfig)` |
| `RedisIdempotencyStore` | empty struct | full impl behind `RedisIdempotencyStore::connect(RedisIdempotencyConfig)` |

### Additions (non-breaking)

- `bus_nats::NatsKvIdempotencyConfig`
- `bus_nats::RedisIdempotencyConfig`
- `bus_nats::ConnectOptions` (re-export of `async_nats::ConnectOptions`)
- `NatsClient::connect_with_options(url, opts, stream_cfg)`
- DLQ stream is auto-created on `subscribe()` when `DlqOptions` is set.

### CHANGELOG.md skeleton

```markdown
## [0.1.1] — 2026-05-XX

### Added
- `NatsClient::connect_with_options` accepting `async_nats::ConnectOptions` for auth, TLS, cluster URLs.
- `bus_nats::ConnectOptions` re-export.
- `NatsKvIdempotencyConfig` for bucket name / replicas / max-age tuning.
- `RedisIdempotencyConfig` and a working `RedisIdempotencyStore` (atomic Lua-based claim).
- DLQ stream is now auto-created when `SubscribeOptions::dlq` is set.

### Changed (breaking)
- `NatsKvIdempotencyStore::new` now takes `NatsKvIdempotencyConfig` instead of `(js, Duration)`.
- README, feature table, and implementation status updated to drop circuit-breaker and SQLite-buffer claims.

### Removed (breaking)
- `bus_nats::circuit_breaker` module — was unused; primitive existed but was never wired into the publisher pipeline. Re-introduce when there is a concrete user need.
- `bus_nats::SqliteBuffer` and the `sqlite-buffer` feature — same rationale.

### Fixed
- DLQ publish was silently failing on the first terminal failure because the per-consumer DLQ stream was never created (the helper `ensure_dlq_stream` existed but was never called). Fixed by calling it from `subscribe()`.

### Migration from 0.1.0
1. **Idempotency store init**: `NatsKvIdempotencyStore::new(js, max_age)` →
   `NatsKvIdempotencyStore::new(js, NatsKvIdempotencyConfig { max_age, ..Default::default() })`.
2. **Circuit breaker**: if you imported `bus_nats::circuit_breaker::*`, copy the file into your own
   crate or vendor it — it is a small, self-contained primitive and the implementation is
   preserved at git tag `v0.1.0`.
3. **SQLite buffer**: same as above; vendor from `v0.1.0` git history if needed.
4. **Redis store**: previously a stub. New API:
   `RedisIdempotencyStore::connect(RedisIdempotencyConfig { url, ..Default::default() }).await?`.
```

### Version bumps

- Workspace `Cargo.toml` `[workspace.package] version = "0.1.0"` → `"0.1.1"`.
- README install snippet `tag = "v0.1.0"` → `"v0.1.1"` (or the actual tag, set at release time).

---

## 7. Out of scope for this pass (deferred to v0.2+)

For traceability, the items below were considered and intentionally deferred:

- **Graceful shutdown for subscriber** — current `Drop::abort()` violates the README §7 "bounded by `ack_wait`" promise. Needs a separate cancellation token + drain mechanism.
- **Publish timeout** — `publisher.rs` awaits `publish_with_headers` and the subsequent ack future without a timeout; a hung NATS server can hang callers forever.
- **`max_ack_pending` / `idle_heartbeat`** on the consumer config — head-of-line blocking and silent disconnect risks under long `ack_wait`.
- **Backoff jitter** in `compute_backoff` — thundering herd risk with many workers.
- **Stream / consumer config reconciliation** — `get_or_create_*` only creates if absent; existing servers retain stale config.
- **DLQ payload / `failure_detail` size cap** — could overflow NATS header limit (~64 KB) if a handler returns a huge error.
- **Dead-end detection for repeatedly-failing DLQ publish** — currently cycles forever.
- **Connection event callbacks** — see §4.5 deferred note.
- **Metrics / Prometheus exporter** — README §8 explicitly defers this.
- **Codec pluggability** — `Publisher::publish` is hardcoded to `serde_json`.
- **IaC-friendly stream provisioning** — `connect()` always calls `ensure_stream`; some shops want to deploy stream config separately.
- **Redaction hooks for DLQ payloads** — PII safety.

A follow-up spec will pick from this list once v0.1.1 ships.

---

## 8. Open questions

None at this time — all design questions were resolved during the brainstorming session leading to this spec.


# Changelog

All notable changes to this workspace are documented here.

## [Unreleased]

## [eventbus-nats 0.1.2] — 2026-05-09

### Added

- `eventbus_nats::nats` namespace re-exporting the entire `bus-nats` crate, so applications no longer
  need to declare `bus-nats` in `Cargo.toml` to reach types like `nats::advisory::*`, `nats::dlq::*`,
  `nats::inbox::*`.
- `eventbus_nats::core` namespace re-exporting `bus-core`, useful for consumers that don't use the
  derive macro and want direct access to traits.
- Top-level shortcuts in `eventbus_nats::*` for the most common transport types: `NatsClient`,
  `StreamConfig`, `SubscribeOptions`, `ConnectOptions`, `NatsPublisher`, `NatsKvIdempotencyConfig`
  (gated by the `nats-kv-inbox` feature), and `RedisIdempotencyConfig`/`RedisIdempotencyStore`
  (gated by `redis-inbox`).

### Changed

- Examples (`examples/01-basic-publish`, `examples/03-idempotent-handler`) now depend only on
  `eventbus-nats` + `bus-core` and import everything through the facade re-exports.

> Note: applications that use `#[derive(Event)]` still need `bus-core` as a peer dependency
> because the proc-macro emits absolute `bus_core::…` paths (the same pattern `serde_derive` uses
> with `serde`). To eliminate `bus-core` from `Cargo.toml` you must implement `Event` manually.

## [Workspace 0.1.1] — 2026-05-08

### Renamed

- Crate **`bus-macros`** was renamed to **`eventbus-macros`** on crates.io (Rust: `eventbus_macros`).
  Workspace path remains `crates/bus-macros/`.
- Crate façade **`event-bus`** could not stay that name on crates.io (occupied). Published name is **`eventbus-nats`**
  (Rust import: **`eventbus_nats`**) — this crate still wires **`bus-nats`** today; use **`bus-core`** alone when you ship a non-NATS backend.

## [0.1.1] — 2026-05-08

### Added

- `NatsClient::connect_with_options` accepting `async_nats::ConnectOptions` for auth, TLS,
  cluster URLs, ping interval, and other tuning.
- `bus_nats::ConnectOptions` re-export so callers don't need to add `async-nats` to their own
  `Cargo.toml`.
- `NatsKvIdempotencyConfig` for tuning bucket name, replica count, and bucket-level TTL on the NATS
  KV idempotency backend.
- `RedisIdempotencyConfig` and a working `RedisIdempotencyStore` (atomic Lua-based `try_claim`,
  `SET PX` for `mark_done`, `DEL` for `release`).
- DLQ stream is auto-created on `subscribe()` when `SubscribeOptions.dlq` is `Some`.

### Changed (breaking)

- `NatsKvIdempotencyStore::new` now takes `(jetstream::Context, NatsKvIdempotencyConfig)` instead of
  `(jetstream::Context, Duration)`. Migration: wrap the old `Duration` in
  `NatsKvIdempotencyConfig { max_age, ..Default::default() }`.
- README, feature table, and implementation status updated to drop circuit-breaker and SQLite-buffer
  claims (the modules were never wired into the publisher pipeline).

### Removed (breaking)

- `bus_nats::circuit_breaker` module — was unused. The implementation remains at git tag `v0.1.0`
  for any downstream user that needs to vendor it.
- `bus_nats::SqliteBuffer` and the `sqlite-buffer` feature — same rationale.
- `event-bus`'s `sqlite-buffer` feature, `EventBusBuilder::sqlite_buffer` method, and prelude
  re-export of `SqliteBuffer`.
- Test `dlq_publish_failure_naks_original_handler_invoked_max_deliver_times` — relied on the DLQ
  stream being absent; with auto-create the failure mode it simulated requires more elaborate fault
  injection that's out of scope.

### Fixed

- DLQ publish was failing on the first terminal failure because the per-consumer DLQ stream was
  never created on `subscribe()`. The helper `ensure_dlq_stream` existed but was unreachable from the
  direct `subscribe()` path. Fix: call `ensure_dlq_stream` from `subscribe()` when `DlqOptions` is
  set. Idempotent w.r.t. operators that pre-provision the stream.

### Migration from 0.1.0

1. **NATS KV idempotency init**:

   ```rust
   // before
   NatsKvIdempotencyStore::new(js, Duration::from_secs(3600)).await?
   // after
   NatsKvIdempotencyStore::new(
       js,
       NatsKvIdempotencyConfig {
           max_age: Duration::from_secs(3600),
           ..Default::default()
       },
   ).await?
   ```

2. **Circuit breaker / SQLite buffer**: vendor the implementations from git tag `v0.1.0` if you were
   depending on them.

3. **Redis idempotency store** (previously a stub):

   ```rust
   let store = RedisIdempotencyStore::connect(RedisIdempotencyConfig {
       url: "redis://localhost:6379".into(),
       ..Default::default()
   }).await?;
   ```

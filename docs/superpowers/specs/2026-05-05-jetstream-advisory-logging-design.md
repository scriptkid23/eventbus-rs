# JetStream advisory logging — Design

**Status:** Approved for implementation planning  
**Date:** 2026-05-05  
**Scope:** `crates/bus-nats` only (`advisory` module)

## 1. Context

JetStream emits **advisory** messages on core NATS subjects under `$JS.EVENT.ADVISORY.*` (e.g. consumer message terminated, stream/consumer lifecycle). These are operational signals, not application events.

`bus-nats` already uses `tracing` heavily on the consume path; `src/advisory.rs` is a stub. `async-nats` 0.46 exposes `jetstream::Context::client()` for plain `subscribe`, which is the correct transport for advisories.

## 2. Goals

- Provide an **opt-in** background task that subscribes to a configurable subject wildcard, receives advisories, and emits **structured `tracing` logs** (parse JSON when possible).
- Default subscription scope is **advisories only**, not `$JS.EVENT.METRIC.>` (metrics are higher-volume and out of scope for this feature).

## 3. Non-goals

- OpenTelemetry metrics, counters, or exporters.
- Persistence, dashboards, or alerting integrations.
- Automatic wiring from `event-bus` crate or `subscribe()` — callers explicitly spawn the logger.
- Strongly typed decoding of every advisory variant (optional follow-up).

## 4. Recommended approach

**Single spawned task** (`spawn_jetstream_advisory_logger`) per process (or per connection), not one task per business consumer. Avoids duplicate logs and matches how NATS fans out advisories.

## 5. Public API (`bus-nats::advisory`)

### 5.1 Types

- **`AdvisoryLogOptions`**
  - `subject_filter: String` — default: `"$JS.EVENT.ADVISORY.>"`.
  - Optional (implement if simple): `max_payload_bytes_log: usize` — cap bytes included in a debug field when logging unparsed/raw summary (default e.g. 256 or 512) to avoid huge log lines.

- **`AdvisoryLoggerHandle`**
  - Holds a `tokio::task::JoinHandle<()>`.
  - On `drop`, **abort** the task (same spirit as `SubscriptionHandle`).

### 5.2 Function

```rust
pub async fn spawn_jetstream_advisory_logger(
    js: &async_nats::jetstream::Context,
    opts: AdvisoryLogOptions,
) -> Result<AdvisoryLoggerHandle, bus_core::error::BusError>
```

- Implementation uses `js.client().clone()` then `Client::subscribe(opts.subject_filter.as_str()).await`.
- Subscribe failure maps to `BusError::Nats(String)`.

Re-export from `bus-nats` `lib.rs` as needed (`pub use advisory::{…}`).

## 6. Logging behavior

### 6.1 Target and level

- **Target:** `bus_nats::jetstream_advisory` (filterable in `tracing-subscriber`).
- **Success path:** `tracing::info!` for each received advisory message.

### 6.2 Structured fields

Always include:

- `subject` (message subject string).

If payload parses as JSON (`serde_json::from_slice`):

- Include `advisory_type` when the value is an object with a string field `"type"` (JetStream JSON advisories commonly expose this).
- Optionally include `stream`, `consumer` when present as top-level string fields in the object (best-effort; names as in payload).

If parse fails:

- `tracing::warn!` with `subject`, `payload_len`, and a short note — **do not** dump full binary payload at warn level. If `max_payload_bytes_log` is set, a **truncated UTF-8 lossy** prefix may appear at `debug!` only (optional; skip if it adds too much code).

### 6.3 Loop errors

If `subscribe` succeeds but the message stream yields a connection-level error (e.g. broken):

- Log `tracing::error!` with the error.
- **Retry:** sleep with exponential backoff (e.g. base 200 ms, factor 2, max 30 s), then attempt `subscribe` again. Continue until task aborted. This avoids tight spin on permanent failure while still recovering after NATS restarts.

## 7. Permissions and operations

Document in the module rustdoc (and optionally one README sentence later):

- The NATS user must be allowed to subscribe to the chosen pattern (typically `$JS.EVENT.ADVISORY.>`).

## 8. Testing

- **Unit tests** in `bus-nats`: pass sample JSON payloads (minimal objects with `"type"` and without) through a package-private `fn log_fields_from_payload(bytes: &[u8]) -> …` or equivalent pure helper so parsing is deterministic without a server.
- **Integration tests** with testcontainers are **not required** for the initial PR; add only if low flake and high value.

## 9. Dependencies

- No new crates: use existing `async-nats`, `tokio`, `tracing`, `serde_json`, `bus-core` (`BusError`).

## 10. Files to touch

- `crates/bus-nats/src/advisory.rs` — implementation.
- `crates/bus-nats/src/lib.rs` — re-exports.
- `crates/bus-nats/Cargo.toml` — no change expected unless tests need dev-only fixtures.

## 11. Risks

| Risk | Mitigation |
|------|------------|
| Log volume in busy clusters | Default excludes METRIC; user narrows wildcard if needed |
| Sensitive data in advisories | Document that payloads are operational; operators filter targets |

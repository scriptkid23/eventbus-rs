# Slim `eventbus-rs` to event-transport only — Design

**Status:** Proposed
**Date:** 2026-05-05
**Scope:** workspace-wide (all crates, README, examples, in-flight specs)
**Supersedes / cancels:** [`2026-05-04-outbox-dispatcher-design.md`](2026-05-04-outbox-dispatcher-design.md)

## 1. Context and goal

Today `eventbus-rs` is a "batteries-included" workspace bundling four
responsibilities: typed events, NATS JetStream transport, transactional Postgres
outbox, and idempotent inbox. The in-flight outbox dispatcher spec
(`2026-05-04-...`) keeps growing — lease-based claim, dead-row recovery,
header allow-list, breaking trait changes — making it clear the outbox alone
warrants a project-sized effort.

This design **resets the project scope**: `eventbus-rs` becomes a focused
event-transport library. Transactional consistency between database writes and
event publishes is **out of scope** and left to application code or a separate
library.

### Why now

Three concerns converge:

1. **Outbox is growing into its own project.** The dispatcher spec already
   touches lease semantics, dead-row recovery, header sanitization, breaking
   trait changes, and a fourth migration. Bundling that with the transport
   makes both harder to evolve independently.
2. **A "true" event bus should not force a database choice.** Coupling to
   `sqlx` / Postgres pushes a heavy infra requirement onto every user, even
   those who only need fire-and-forget pub/sub.
3. **Multi-transport portability.** A future `bus-kafka` or `bus-redis-streams`
   backend has to re-think the entire transactional story if outbox stays
   bundled. Decoupling now keeps each transport adapter small.

### Goals

- Reduce `eventbus-rs` to: typed events + transport (NATS) + inbox idempotency
  (NATS-KV / Redis) + DLQ + circuit breaker + offline buffer.
- Delete all transactional / DB-coupled code from this workspace.
- Keep `bus-core` traits unchanged so future external libraries (including a
  user-built outbox) can extend without forking.
- README and examples accurately reflect the new scope; no "ghost features".

### Non-goals

- Shipping any outbox replacement (separate crate, sidecar, or sample).
- Adding a second transport adapter (Kafka / Redis Streams) — kept as
  future roadmap, not part of this change.
- Deprecation period for `bus-outbox` users. The crate has never been
  published to `crates.io`; there are no external consumers.

## 2. New positioning

| | Before | After |
|---|---|---|
| Tagline | "production-grade async event bus for Rust — NATS JetStream + transactional Postgres outbox + idempotent inbox" | "typed async event bus for Rust — NATS JetStream with idempotent inbox, DLQ, and circuit breaker" |
| Core promise | Effectively-once delivery + atomic publish-with-DB-write | Effectively-once delivery |
| Required infra | NATS + (optionally) Postgres + (optionally) Redis | NATS + (optionally) Redis |
| Top-level concerns | Events, transport, outbox, inbox | Events, transport, inbox |

The "effectively-once" promise is preserved: JetStream `Nats-Msg-Id` dedup on
publish + `IdempotencyStore::try_claim` on consume still collapse duplicates
end-to-end. What is removed is the **atomic publish-with-DB-write** guarantee —
that becomes the user's responsibility.

## 3. Crate layout after slim

```
crates/
├── bus-core         (UNCHANGED — traits)
├── bus-macros       (UNCHANGED — #[derive(Event)])
├── bus-nats         (+ sqlite_buffer.rs moved from bus-outbox)
├── bus-telemetry    (drop outbox-related planned metrics)
└── event-bus        (slim feature flags; no outbox)

REMOVED:
└── bus-outbox       (entire crate deleted)
```

### 3.1 `bus-core` — unchanged

The trait surface (`Event`, `MessageId`, `Publisher`, `EventHandler`,
`HandlerCtx`, `IdempotencyStore`, `ClaimOutcome`, `BusError`, `HandlerError`,
`PubReceipt`) stays exactly as today. This is the load-bearing extension point
for any future external outbox or alternative transport.

### 3.2 `bus-macros` — unchanged

`#[derive(Event)]` and the compile-fail snapshot tests stay as-is.

### 3.3 `bus-nats` — gains SQLite buffer

The SQLite offline-queue fallback for `CircuitBreaker` is a transport concern
(it spools publishes when NATS is unreachable, replays on recovery). It is
**not** a transactional outbox. Move it from `bus-outbox` to `bus-nats` so all
"transport-level offline queue + circuit breaker" code lives together:

| From | To |
|---|---|
| `crates/bus-outbox/src/sqlite.rs` | `crates/bus-nats/src/sqlite_buffer.rs` |
| `bus-outbox` `Cargo.toml` `[features] sqlite-buffer` | `bus-nats` `Cargo.toml` `[features] sqlite-buffer` |
| `rusqlite` workspace dep usage in `bus-outbox` | same dep, consumed by `bus-nats` instead |

Public re-exports from `bus-nats::lib` are extended to surface the buffer
under the `sqlite-buffer` feature. The `CircuitBreaker` integration point is
the same — only the file location changes.

`bus-nats` continues to host:

- `NatsClient`, `NatsPublisher`, pull subscriber + retry + DLQ
- `CircuitBreaker` (Closed / Open / HalfOpen)
- `NatsKvIdempotencyStore` (default), `RedisIdempotencyStore`
- **NEW:** SQLite offline buffer (gated `sqlite-buffer`)

### 3.4 `bus-telemetry` — drop outbox metrics

`README` lists planned metrics including `eventbus.outbox.pending`. Remove
that row from the metrics table. Keep all other planned metrics (publish,
consume, handle duration, DLQ, JetStream advisories).

### 3.5 `event-bus` — slim facade

The builder/facade no longer references outbox. Feature flags shrink (see §4).
No code change is expected beyond removing outbox-related glue — which the
facade does not currently have, since the dispatcher integration was still in
the in-flight spec.

### 3.6 `bus-outbox` — deleted

Entire crate removed:

- `crates/bus-outbox/src/{dispatcher,inbox_pg,lib,migrate,postgres,sqlite,store}.rs`
  - `sqlite.rs` is moved to `bus-nats` *before* the crate is deleted (see §3.3).
- `crates/bus-outbox/migrations/`
- `crates/bus-outbox/tests/`
- `crates/bus-outbox/Cargo.toml`
- Workspace member entry in root `Cargo.toml`

`PostgresIdempotencyStore` is deleted with the crate. NATS-KV and Redis stores
remain as the supported `IdempotencyStore` implementations. Users who need a
Postgres-backed inbox can implement the trait themselves against their own
schema.

## 4. Cargo features after slim

| Crate | Feature | Default | Description |
|---|---|---|---|
| `event-bus` | `macros` | yes | Re-export `#[derive(Event)]` from `bus-macros` |
| `event-bus` | `nats-kv-inbox` | yes | NATS KV-backed `IdempotencyStore` |
| `event-bus` | `redis-inbox` | no | Redis-backed `IdempotencyStore` |
| `event-bus` | `sqlite-buffer` | no | Local-disk fallback buffer for offline publishing |
| `event-bus` | `otel` | no | OpenTelemetry spans + metrics (via `bus-telemetry`) |
| `bus-nats` | `nats-kv-inbox` | yes | (mirrored, transitive) |
| `bus-nats` | `redis-inbox` | no | (mirrored, transitive) |
| `bus-nats` | `sqlite-buffer` | no | (mirrored, transitive) |

**Removed features:** `postgres-outbox`, `postgres-inbox`, `saga`.

Feature-flag wiring in `event-bus/Cargo.toml` is updated so that
`event-bus/sqlite-buffer` enables `bus-nats/sqlite-buffer` (was:
`bus-outbox/sqlite-buffer`).

## 5. Files removed

- `crates/bus-outbox/` — entire directory (after `sqlite.rs` is moved out)
- `examples/02-outbox-postgres/` — entire directory; remove from workspace
  `members`
- `docs/superpowers/specs/2026-05-04-outbox-dispatcher-design.md` — superseded
  by this spec; deletion is recorded in this spec's commit message

## 6. README rewrite

The README is the public face of the project, so the rewrite has to be
consistent: every mention of outbox, dispatcher, saga, Postgres-as-required
infra, and the v0.2 dispatcher milestone must go.

### 6.1 Sections to change

| Section | Change |
|---|---|
| Title block + tagline | Replace per §2 |
| Lead paragraph | Drop "transactional outbox" bullet; rewrite four-primitives list as three (typed events, idempotent inbox, DLQ + circuit breaker + SQLite fallback) |
| Features list | Remove "Transactional outbox" line; remove planned-saga implication |
| Installation feature list | Remove `postgres-outbox`, `postgres-inbox` from default `features = […]` example; drop `sqlx` from required peer deps |
| Quick start | **No change** (already pure pub/sub with NATS-KV inbox) |
| §1 Connect with cluster URLs | No change |
| §2 Configure JetStream | No change |
| §3 Pick an idempotency backend | Drop the Postgres row from the table; drop the Postgres `# #[cfg(feature = "postgres-inbox")]` snippet; tighten copy ("two backends: NATS KV default, Redis when you want lower-latency `SET NX EX`") |
| §4 Use the transactional outbox | **Replaced** by §4 "Transactional publishing" disclaimer (see §6.2 below) |
| §5 Run the outbox dispatcher | **Removed**; subsequent sections renumbered |
| §6 Subscribe with retry, DLQ | Renumbered to §5; no content change |
| §7 Handle errors | Renumbered to §6; no content change |
| §8 Graceful shutdown | Renumbered to §7; no content change |
| §9 Observability | Renumbered to §8; remove `eventbus.outbox.pending` row from metrics table |
| Cargo features | Replace per §4 |
| Architecture mermaid | Drop the entire outbox subgraph (`tx`, `postgres_outbox`, `dispatcher`); keep `publish → event_bus → bus_nats → jetstream → pull_consumer → idempotency → handler` |
| Implementation status table | Remove rows: Postgres outbox store, Postgres idempotency store, Outbox dispatcher, Saga engine. Relocate "SQLite fallback buffer" row from `bus-outbox` (`sqlite-buffer`) to `bus-nats` (`sqlite-buffer`) |
| Examples table | Remove `examples/02-outbox-postgres` row |
| FAQ | Remove "Do I need the outbox if I'm already using JetStream?"; simplify "Can I use this without Postgres?" to a one-line "Yes — NATS-KV or Redis idempotency cover all supported deployments." |
| Roadmap | v0.2 = `crates.io` publish + OTel; v0.3 = (optional) Kafka backend; remove saga and multi-DB outbox |
| Contributing local checks | Remove `cargo test -p bus-outbox --features sqlite-buffer`; replace with `cargo test -p bus-nats --features sqlite-buffer` |

### 6.2 New §4 "Transactional publishing" — verbatim wording target

The replacement section is intentionally short and link-free. Suggested
content:

> ### 4. Transactional publishing
>
> `bus.publish()` writes directly to NATS. If you need an event publish to
> reflect a database mutation atomically (so a crash between commit and
> publish cannot drop the event), `eventbus-rs` does **not** ship that
> mechanism — implement the outbox pattern in your application: write an
> outbox row in the same transaction as the business write, then have a
> separate task read pending rows and call `Publisher::publish` from
> `bus-core`. The traits are designed to support this without forking.

No code sample, no library recommendation, no schema. The point is to set
correct expectations without taking on maintenance of an outbox pattern.

## 7. Versioning

Stay at `0.1.0`. The workspace has never been published to `crates.io`, so
there are no downstream version constraints to honour. Bumping is symbolic
and would burn a version number for no consumer benefit. The first
`crates.io` publish (currently a roadmap item) becomes the meaningful
version marker.

## 8. Sequencing

This is a single coherent change shipped as one PR. The intermediate states
between steps must keep `cargo build --workspace --all-features` and
`cargo test --workspace` passing.

1. **Move SQLite buffer.** Copy `crates/bus-outbox/src/sqlite.rs` to
   `crates/bus-nats/src/sqlite_buffer.rs`. Move any SQLite-specific tests
   from `crates/bus-outbox/tests/` to `crates/bus-nats/tests/` (audit at
   move time — keep test names; rewire `use` paths). Add `mod sqlite_buffer`
   and re-exports under `#[cfg(feature = "sqlite-buffer")]` in
   `bus-nats/lib.rs`. Add the `sqlite-buffer` feature to `bus-nats/Cargo.toml`
   with the `rusqlite` optional dep. Update `event-bus/Cargo.toml` so
   `sqlite-buffer = ["bus-nats/sqlite-buffer"]`.
2. **Verify move compiles.** `cargo build --workspace --features sqlite-buffer`.
3. **Delete `bus-outbox` crate.** Remove `crates/bus-outbox/` directory.
   Remove `"crates/bus-outbox"` from root `Cargo.toml` `workspace.members`.
   Remove `postgres-outbox`, `postgres-inbox` features from
   `event-bus/Cargo.toml`.
4. **Delete `examples/02-outbox-postgres/`.** Remove the directory and the
   `"examples/02-outbox-postgres"` workspace member entry.
5. **Delete dispatcher spec.** Remove
   `docs/superpowers/specs/2026-05-04-outbox-dispatcher-design.md`.
6. **Rewrite README per §6.**
7. **Drop `eventbus.outbox.pending`** row from the README metrics table
   (already covered in §6, called out separately for `bus-telemetry` planning
   docs if any exist).
8. **Verify clean.** `cargo fmt --all`, `cargo clippy --workspace
   --all-features -- -D warnings`, `cargo build --workspace --all-features`,
   `cargo test --workspace`.
9. **Single commit.** Message explains scope reset, lists removed crates /
   features, and references this spec.

## 9. What this change does NOT do

To prevent scope creep during implementation:

- Does not modify `bus-core` traits, `bus-macros`, `bus-nats` publisher /
  subscriber / DLQ / circuit-breaker, or `event-bus` builder logic beyond
  feature-flag wiring and the SQLite-buffer move.
- Does not rename anything in the public API.
- Does not add new tests for SQLite buffer beyond what already lives in
  `bus-outbox/tests/` for that file (move the relevant tests with the file).
- Does not start a Kafka backend, Redis Streams backend, or any saga work.
- Does not bump the workspace version.
- Does not publish to `crates.io`.

## 10. Risks and mitigations

| Risk | Mitigation |
|---|---|
| Hidden coupling: `event-bus` facade silently re-exports something from `bus-outbox` | Step 3 of §8 will fail to compile if so; fix call sites then |
| SQLite buffer's `CircuitBreaker` integration relied on `bus-outbox` types | Audit during step 1; the buffer should depend only on its own types + `bus-core::BusError`. If it pulls anything else from `bus-outbox`, copy or trim during the move |
| External users have a fork or path-dep on `bus-outbox` | Acceptable: never published, no consumers known. Spec change is announced via the commit message |
| README references in other docs (`docs/diagrams/`) reference outbox | Audit during step 6; update or note as out-of-scope follow-up |
| Loss of "atomic publish-with-DB-write" surprises a user later | §6.2 disclaimer is the primary mitigation; the trait surface in `bus-core` allows them to roll their own without forking |

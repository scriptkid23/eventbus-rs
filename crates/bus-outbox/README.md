# bus-outbox

Transactional outbox + idempotency inbox stores for `eventbus-rs`.

## What this crate provides

- **`PostgresOutboxStore`** — write events into an `eventbus_outbox` table inside the same SQL transaction that mutates business state. A relay loop reads pending rows and publishes them to NATS, retrying on failure.
- **`PostgresIdempotencyStore`** — subscriber-side store that records claim state (`pending` / `done`) per `(consumer, message_id)` so handler invocations are deduplicated even after redelivery.
- **`SqliteBuffer`** *(feature `sqlite-buffer`)* — local-disk durability buffer for publishing when NATS is unreachable. Not an outbox — a fallback queue.

## Why outbox is database-bound

The transactional outbox pattern relies on a single transaction that atomically writes both business state and the outbox event row:

```sql
BEGIN;
  INSERT INTO orders (...);             -- business write
  INSERT INTO eventbus_outbox (...);    -- event write
COMMIT;
```

If the two writes are split across separate stores, atomicity is lost and either the event or the business mutation can disappear without the other. **The outbox table must therefore live in the same database as business state.**

This makes outbox plug-and-play different from idempotency:

| Component | Co-location requirement |
|-----------|-------------------------|
| `IdempotencyStore` (subscriber) | None — any backend works (NATS KV, Postgres, Redis, in-memory) |
| `OutboxStore` (publisher) | Must share the business transaction → service picks an impl that matches their business DB |

Library responsibility: ship one impl per supported business DB. Service responsibility: pick the impl that matches their stack.

## Current support

**v1 ships Postgres only.** Other backends are deferred — see roadmap below.

The `OutboxStore` trait is currently typed against `sqlx::Transaction<'_, sqlx::Postgres>`. Services using a different database cannot implement the trait today; they would need to either (a) wait for the roadmap entry that matches their stack, or (b) bypass the trait and write directly to an `eventbus_outbox` table they manage themselves, then run their own relay.

## Roadmap

### Phase 2 — generalize `OutboxStore` over `sqlx::Database`

Rework the trait to be generic:

```rust
pub trait OutboxStore<DB: sqlx::Database>: Send + Sync {
    async fn insert<'tx, E: Event>(
        &self,
        tx: &mut sqlx::Transaction<'tx, DB>,
        event: &E,
    ) -> Result<(), BusError>;
    // ...
}
```

Add impls behind cargo features:

- `SqliteOutboxStore` *(feature `sqlite-outbox`)* — for services with a SQLite business DB. Cheap to add; sqlx-sqlite is one feature flag away.
- `MySqlOutboxStore` *(feature `mysql-outbox`)* — for services on MySQL/MariaDB.

Each impl ships its own migration (UUID encoding and JSON column types differ between Postgres / SQLite / MySQL).

### Phase 3 — NoSQL backends

NoSQL outbox is not a drop-in port of the SQL design. Each engine has its own atomicity model and the outbox shape has to follow:

- **MongoDB** — Multi-document transactions on a replica set or sharded cluster (4.0+). Outbox lives as a separate collection in the same database; `insert_one` for business doc + `insert_one` for outbox doc inside `with_transaction`. Practical to support.
- **DynamoDB** — `TransactWriteItems` covers up to 100 items across tables in one region. Outbox is a separate table; service-side puts both writes in the same transact-call. No long-running cursor support → relay polls `GSI(status, created_at)`.
- **Cassandra / ScyllaDB** — Logged batches give partition-level atomicity, not full ACID. Outbox row must share the same partition key as the business row, which constrains schema. Lightweight transactions (Paxos) are an alternative but expensive.
- **Firestore** — `runTransaction` covers ≤ 500 writes; outbox doc lives in the same database. Limited by Firestore's eventual consistency on queries, so the relay needs a status index.
- **FoundationDB / TigerBeetle** — Full ACID, outbox is straightforward; lower adoption.

Each of these warrants its own crate (`bus-outbox-mongo`, `bus-outbox-dynamo`, …) rather than a single generic abstraction, because the transaction primitive differs too much to express in one trait. The `bus-outbox` crate may grow a thin facade trait that all backends implement, but the per-engine impls live in their own crates so services only depend on what they use.

No fixed ETA. Phase 3 work starts when there is a concrete service driving a specific backend.

### Open question for phase 2/3

Should `OutboxStore::insert` stay tightly typed (`sqlx::Transaction<DB>`) or evolve to take a service-owned executor handle so non-sqlx ORMs (Diesel, SeaORM) can also implement it? Decision deferred until a real consumer needs it — the current sqlx-only shape is the simplest thing that works and avoids speculative abstraction.

## Idempotency store generalization

`PostgresIdempotencyStore` is also Postgres-only at the impl level, but the `IdempotencyStore` trait in `bus-core` is database-agnostic. Services that don't need a co-located idempotency store can use `NatsKvIdempotencyStore` (default) or implement the trait themselves. There is therefore no urgent need to add SQLite/MySQL idempotency impls; they are a phase-2 nice-to-have, not a blocker.

## Usage today

Default features include `postgres-outbox` and `postgres-inbox`. A service on Postgres needs nothing extra:

```toml
event-bus = { version = "*", features = ["postgres-outbox", "postgres-inbox"] }
```

A service on a non-Postgres backend should disable these features and either use the NATS-KV idempotency store or wait for the relevant roadmap entry:

```toml
event-bus = { version = "*", default-features = false, features = ["nats-kv-inbox"] }
```

See the workspace root [README](../../README.md) for end-to-end builder examples.

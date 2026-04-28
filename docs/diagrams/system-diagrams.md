# eventbus-rs — System Diagrams

## 1. Full Workspace Crate Dependency Graph

```mermaid
graph TD
    subgraph Workspace["Cargo Workspace — eventbus-rs"]
        BC["bus-core<br/>traits · types · errors<br/><i>zero workspace deps</i>"]
        BM["bus-macros<br/>#[derive(Event)]<br/><i>proc-macro = true</i>"]
        BN["bus-nats<br/>JetStream publisher<br/>pull consumer · DLQ<br/>circuit breaker · inbox"]
        BO["bus-outbox<br/>Postgres outbox<br/>Postgres inbox<br/>SQLite buffer"]
        BT["bus-telemetry<br/>OTel spans · metrics<br/>W3C traceparent"]
        EB["event-bus<br/>public facade<br/>EventBusBuilder · EventBus<br/>saga engine · prelude"]
    end

    BM -->|dep| BC
    BN -->|dep| BC
    BO -->|dep| BC
    BT -->|dep| BC
    EB -->|dep| BC
    EB -->|dep| BN
    EB -->|optional: postgres-outbox| BO
    EB -->|optional: otel| BT
    EB -->|optional: macros| BM

    subgraph ExtDeps["Key External Dependencies"]
        AN["async-nats 0.46"]
        SX["sqlx 0.8 (Postgres)"]
        RS["rusqlite 0.31"]
        OT["opentelemetry 0.24"]
        TK["tokio 1.36"]
        SE["serde 1"]
        UU["uuid 1 (v7)"]
        SN["syn 2 · quote · proc-macro2"]
    end

    BN --> AN
    BO --> SX
    BO --> RS
    BT --> OT
    BC --> SE
    BC --> UU
    BM --> SN
    BN --> TK
    BO --> TK
    EB --> TK
```

---

## 2. Feature Flag Map — `event-bus` crate

```mermaid
graph LR
    EB["event-bus"]

    EB -->|default ON| MAC["macros<br/>→ bus-macros<br/>#[derive(Event)]"]
    EB -->|default ON| NKV["nats-kv-inbox<br/>→ bus-nats/inbox/nats_kv<br/>IdempotencyStore via NATS KV"]
    EB -->|opt-in| PGI["postgres-inbox<br/>→ bus-outbox/inbox_pg<br/>IdempotencyStore via PostgreSQL"]
    EB -->|opt-in| RDI["redis-inbox<br/>→ bus-nats/inbox/redis<br/>IdempotencyStore via Redis"]
    EB -->|opt-in| PGO["postgres-outbox<br/>→ bus-outbox/postgres<br/>Transactional Outbox"]
    EB -->|opt-in| SLB["sqlite-buffer<br/>→ bus-outbox/sqlite<br/>NATS-down fallback buffer"]
    EB -->|opt-in| OTL["otel<br/>→ bus-telemetry<br/>Full OpenTelemetry"]
    EB -->|opt-in| SAG["saga<br/>implies postgres-outbox<br/>Orchestration state machine"]

    style MAC fill:#d4edda
    style NKV fill:#d4edda
    style PGI fill:#fff3cd
    style RDI fill:#fff3cd
    style PGO fill:#fff3cd
    style SLB fill:#fff3cd
    style OTL fill:#fff3cd
    style SAG fill:#fff3cd
```

---

## 3. bus-core — Complete Type & Trait Map

```mermaid
classDiagram
    class MessageId {
        -Uuid inner
        +new() MessageId
        +from_uuid(u: Uuid) MessageId
        +as_uuid() &Uuid
        +to_string() String
        +from_str(s: &str) Result~MessageId~
        +PartialOrd / Ord / Hash
        +Serialize / Deserialize
    }

    class Event {
        <<trait>>
        +subject(&self) Cow~str~
        +message_id(&self) MessageId
        +aggregate_type() &'static str
        --
        bounds: Serialize + DeserializeOwned
        bounds: Send + Sync + 'static
    }

    class PubReceipt {
        +stream: String
        +sequence: u64
        +duplicate: bool
        +buffered: bool
    }

    class Publisher {
        <<trait>>
        +publish~E:Event~(&self, event) Result~PubReceipt~
        +publish_batch~E:Event~(&self, events) Result~Vec~PubReceipt~~
        --
        NOTE: not object-safe (generic methods)
        bounds: Send + Sync
    }

    class HandlerCtx {
        +msg_id: MessageId
        +stream_seq: u64
        +delivered: u64
        +subject: String
        +span: tracing::Span
    }

    class EventHandler {
        <<trait>>
        +handle(ctx: HandlerCtx, event: E) Result~(), HandlerError~
        --
        bounds: Send + Sync + 'static
    }

    class IdempotencyStore {
        <<trait>>
        +try_insert(key: &MessageId, ttl: Duration) Result~bool~
        +mark_done(key: &MessageId) Result~()~
        --
        MUST be atomic (INSERT ON CONFLICT,
        KV.Create, SET NX)
        bounds: Send + Sync
    }

    class BusError {
        <<enum>>
        Nats(String)
        Publish(String)
        Outbox(String)
        Idempotency(String)
        Serde(serde_json::Error)
        Handler(HandlerError)
        NatsUnavailable
    }

    class HandlerError {
        <<enum>>
        Transient(String) → NAK + retry
        Permanent(String) → Term + DLQ
    }

    Event --> MessageId : message_id() returns
    Publisher --> Event : generic over E: Event
    Publisher --> PubReceipt : returns on success
    Publisher --> BusError : returns on error
    EventHandler --> Event : generic over E: Event
    EventHandler --> HandlerCtx : receives
    EventHandler --> HandlerError : returns on failure
    IdempotencyStore --> MessageId : key type
    IdempotencyStore --> BusError : returns on error
    BusError --> HandlerError : wraps via From
```

---

## 4. bus-nats — Internal Module Structure

```mermaid
graph TD
    LIB["lib.rs<br/>(pub re-exports)"]

    LIB --> CLI["client.rs<br/>NatsClient<br/>connect + ensure_stream"]
    LIB --> STR["stream.rs<br/>StreamConfig<br/>ensure_stream()"]
    LIB --> PUB["publisher.rs<br/>NatsPublisher<br/>impl Publisher"]
    LIB --> ACK["ack.rs<br/>double_ack()<br/>nak_with_delay()<br/>term()"]
    LIB --> CON["consumer.rs<br/>build_pull_config()"]
    LIB --> SUB["subscriber.rs<br/>subscribe()<br/>SubscribeOptions<br/>SubscriptionHandle"]
    LIB --> CB["circuit_breaker.rs<br/>CircuitBreaker<br/>sliding window"]
    LIB --> DLQ["dlq.rs<br/>DLQ republish"]
    LIB --> ADV["advisory.rs<br/>MAX_DELIVERIES<br/>MSG_TERMINATED"]
    LIB --> INB["inbox/mod.rs"]

    INB --> KV["inbox/nats_kv.rs<br/>NatsKvIdempotencyStore<br/>feature: nats-kv-inbox"]
    INB --> RD["inbox/redis.rs<br/>RedisIdempotencyStore<br/>feature: redis-inbox"]

    CLI --> STR
    SUB --> ACK
    SUB --> CON
    SUB --> CB
    PUB --> CLI
```

---

## 5. bus-outbox — Internal Module Structure

```mermaid
graph TD
    LIB["lib.rs<br/>(pub re-exports)"]

    LIB --> ST["store.rs<br/>OutboxStore trait<br/>OutboxRow"]
    LIB --> PG["postgres.rs<br/>PostgresOutboxStore<br/>feature: postgres-outbox"]
    LIB --> DS["dispatcher.rs<br/>polling loop<br/>SELECT FOR UPDATE SKIP LOCKED"]
    LIB --> IP["inbox_pg.rs<br/>PostgresIdempotencyStore<br/>feature: postgres-inbox"]
    LIB --> SQ["sqlite.rs<br/>SqliteBuffer<br/>feature: sqlite-buffer"]
    LIB --> MG["migrate.rs<br/>run_migrations()"]

    MIG1["migrations/001_outbox.sql<br/>eventbus_outbox table"]
    MIG2["migrations/002_inbox.sql<br/>eventbus_inbox table"]
    MIG3["migrations/003_sagas.sql<br/>eventbus_sagas table"]

    MG --> MIG1
    MG --> MIG2
    MG --> MIG3

    PG --> ST
    DS --> PG
    IP --> MG
    PG --> MG
```

---

## 6. event-bus Facade — Internal Structure

```mermaid
graph TD
    LIB["lib.rs"]

    LIB --> BLD["builder.rs<br/>EventBusBuilder<br/>url · stream_config<br/>idempotency (required)<br/>sqlite_buffer · with_otel"]

    LIB --> BUS["bus.rs<br/>EventBus<br/>publish()<br/>publish_in_tx()<br/>subscribe()<br/>shutdown()"]

    LIB --> PRE["prelude.rs<br/>re-exports all public API<br/>Event · MessageId · BusError<br/>EventHandler · HandlerCtx ..."]

    LIB --> SAG["saga/mod.rs"]
    SAG --> CHO["saga/choreography.rs<br/>ChoreographyStep trait<br/>Output: Event<br/>Compensation: Event"]
    SAG --> ORC["saga/orchestration.rs<br/>SagaDefinition trait<br/>SagaTransition enum<br/>SagaHandle"]

    BLD -->|builds| BUS
    BUS -->|uses| NP["NatsPublisher (bus-nats)"]
    BUS -->|uses| IS["IdempotencyStore (bus-core)"]
    BUS -->|optional| OS["OutboxStore (bus-outbox)"]
    BUS -->|optional| MT["BusMetrics (bus-telemetry)"]
```

---

## 7. End-to-End Publish Flow (with Outbox)

```mermaid
sequenceDiagram
    participant App as Application
    participant TX as Postgres Transaction
    participant OB as eventbus_outbox
    participant DS as Outbox Dispatcher
    participant JS as NATS JetStream R3
    participant CB as Circuit Breaker

    App->>TX: BEGIN
    App->>TX: INSERT INTO orders (business write)
    App->>TX: bus.publish_in_tx(&mut tx, &event)
    TX->>OB: INSERT INTO eventbus_outbox (id=UUIDv7, subject, payload)
    App->>TX: COMMIT

    Note over DS: background task polls every 250ms
    DS->>OB: SELECT ... FOR UPDATE SKIP LOCKED LIMIT 100
    OB-->>DS: outbox rows

    loop for each row
        DS->>CB: allow_request()?
        CB-->>DS: Closed → true
        DS->>JS: publish_with_headers(Nats-Msg-Id = row.id)
        JS-->>DS: PubAck { sequence, duplicate }
        DS->>OB: UPDATE published_at = now()
    end

    Note over CB: if NATS unavailable
    DS->>CB: record_failure()
    CB-->>DS: Open → false
    DS->>SQ: SqliteBuffer.insert(event)
    Note over SQ: relay task drains on NATS recovery
```

---

## 8. End-to-End Consume Flow (Pull Consumer)

```mermaid
flowchart TD
    JS["NATS JetStream<br/>pull consumer fetch"]
    JS --> MSG["Message received"]

    MSG --> EXT["Extract Nats-Msg-Id header<br/>→ MessageId<br/>(fallback: stream_seq)"]

    EXT --> IDEM{"IdempotencyStore<br/>try_insert(msg_id)"}

    IDEM -->|Ok(false) — duplicate| SKIP["double_ack → skip<br/>(already processed)"]
    IDEM -->|Err — store unavailable| NAK1["NAK(1s delay)<br/>→ redeliver"]
    IDEM -->|Ok(true) — new| DESER["serde_json::from_slice<br/>→ Event"]

    DESER -->|Err — bad payload| TERM1["AckTerm<br/>(no retry for bad data)"]
    DESER -->|Ok| HAND["handler.handle(ctx, event)"]

    HAND -->|Ok| DONE["mark_done(msg_id)<br/>→ double_ack"]
    HAND -->|Err(Transient)| NAКB["NAK(backoff delay)<br/>1s → 5s → 30s → 5m"]
    HAND -->|Err(Permanent)| TERM2["AckTerm<br/>→ republish to DLQ stream"]

    style SKIP fill:#d4edda
    style DONE fill:#d4edda
    style TERM1 fill:#f8d7da
    style TERM2 fill:#f8d7da
    style NAK1 fill:#fff3cd
    style NAКB fill:#fff3cd
```

---

## 9. Circuit Breaker State Machine

```mermaid
stateDiagram-v2
    [*] --> Closed

    Closed --> Open : failure_rate > 50%\nover last N requests
    Open --> HalfOpen : reset_timeout elapsed\n(default: 5s)
    HalfOpen --> Closed : probe success
    HalfOpen --> Open : probe failure

    note right of Closed
        Normal path:
        requests → NATS JetStream
    end note

    note right of Open
        Degraded path:
        requests → SQLite buffer
        background relay waits for recovery
    end note

    note right of HalfOpen
        Probe path:
        one request allowed through
        if success → drain SQLite buffer
    end note
```

---

## 10. Idempotency Store Implementations

```mermaid
classDiagram
    class IdempotencyStore {
        <<trait bus-core>>
        +try_insert(key, ttl) Result~bool~
        +mark_done(key) Result~()~
    }

    class NatsKvIdempotencyStore {
        -kv::Store store
        +new(js, max_age) Self
        --
        atomic: kv.create() fails if exists
        bucket: "eventbus_processed"
        feature: nats-kv-inbox
    }

    class PostgresIdempotencyStore {
        -PgPool pool
        -String consumer
        +new(pool, consumer) Self
        --
        atomic: INSERT ON CONFLICT DO NOTHING
        table: eventbus_inbox
        keyed by: (consumer, message_id)
        feature: postgres-inbox
    }

    class RedisIdempotencyStore {
        +new(client) Self
        --
        atomic: SET NX EX
        feature: redis-inbox
    }

    NatsKvIdempotencyStore ..|> IdempotencyStore
    PostgresIdempotencyStore ..|> IdempotencyStore
    RedisIdempotencyStore ..|> IdempotencyStore
```

---

## 11. PostgreSQL Schema

```mermaid
erDiagram
    eventbus_outbox {
        UUID id PK
        TEXT aggregate_type
        TEXT aggregate_id
        TEXT subject
        JSONB payload
        JSONB headers
        TIMESTAMPTZ created_at
        TIMESTAMPTZ published_at
        INT attempts
        TEXT last_error
    }

    eventbus_inbox {
        TEXT message_id PK
        TEXT consumer PK
        TIMESTAMPTZ processed_at
        TEXT status
    }

    eventbus_sagas {
        TEXT saga_id PK
        TEXT saga_type
        JSONB state
        TEXT status
        TIMESTAMPTZ created_at
        TIMESTAMPTZ updated_at
        INT version
    }
```

---

## 12. Saga Engine — Orchestration State Machine

```mermaid
stateDiagram-v2
    [*] --> running : start_saga(initial_state)

    running --> running : Advance\nnext_state + emit commands
    running --> compensating : Compensate\nemit compensation events
    running --> completed : Complete
    running --> failed : Fail(reason)

    compensating --> failed : all compensations sent

    note right of running
        SagaTransition::Advance:
        UPDATE sagas SET state=next_state, version=version+1
        WHERE version=expected (optimistic lock)
        + emit events via outbox
    end note

    note right of compensating
        SagaTransition::Compensate:
        emit rollback events to
        upstream services
    end note
```

---

## 13. bus-telemetry — OTel Instrumentation Points

```mermaid
graph LR
    subgraph Publish["Publisher path"]
        P1["inject_context(headers)<br/>W3C traceparent → NATS header"]
        P2["eventbus.publish span<br/>attrs: subject · msg_id · stream"]
        P3["eventbus.publish.total counter<br/>eventbus.publish.duration_ms histogram"]
    end

    subgraph Consume["Consumer path"]
        C1["extract_context(headers)<br/>NATS header → parent span"]
        C2["eventbus.receive span<br/>attrs: subject · stream_seq · delivered"]
        C3["eventbus.handle span<br/>attrs: msg_id · handler.name"]
        C4["eventbus.idempotency.check span<br/>attrs: msg_id · result(new/duplicate)"]
        C5["eventbus.consume.total counter<br/>eventbus.redeliveries.total counter<br/>eventbus.dlq.total counter"]
    end

    subgraph Outbox["Outbox dispatcher path"]
        O1["eventbus.outbox.dispatch span<br/>attrs: outbox.id · attempts"]
        O2["eventbus.outbox.dispatch_ms histogram<br/>eventbus.outbox.pending gauge"]
    end

    P1 --> P2 --> P3
    C1 --> C2 --> C3
    C3 --> C4 --> C5
    O1 --> O2
```

---

## 14. W3C Traceparent Cross-Service Propagation

```mermaid
sequenceDiagram
    participant SVC_A as Service A (publisher)
    participant NATS as NATS JetStream
    participant SVC_B as Service B (consumer)
    participant JAEGER as Jaeger / OTLP collector

    SVC_A->>SVC_A: active span: traceid=4bf9...
    SVC_A->>NATS: publish(headers: traceparent=00-4bf9...-00f0...-01)
    NATS-->>SVC_B: deliver message with traceparent header
    SVC_B->>SVC_B: extract_context(headers) → parent span context
    SVC_B->>SVC_B: create child span (traceid=4bf9... same trace!)
    SVC_B->>JAEGER: export spans

    Note over SVC_A,JAEGER: Both services appear in the same Jaeger trace
```

---

## 15. Implementation Plan Sequence (all 3 plans)

```mermaid
gantt
    title eventbus-rs v1.0 Implementation Roadmap
    dateFormat  X
    axisFormat  Plan %s

    section Plan 1 — bus-core + bus-macros
    Workspace Cargo.toml        :p1t1, 0, 1
    Scaffold bus-core            :p1t2, 1, 2
    MessageId (TDD)              :p1t3, 2, 3
    BusError + HandlerError (TDD):p1t4, 3, 4
    Event trait                  :p1t5, 4, 5
    Publisher + PubReceipt       :p1t6, 5, 6
    EventHandler + HandlerCtx    :p1t7, 6, 7
    IdempotencyStore             :p1t8, 7, 8
    Scaffold bus-macros          :p1t9, 8, 9
    derive(Event) impl (TDD)     :p1t10, 9, 10
    Compile-fail tests           :p1t11, 10, 11
    Full workspace check         :p1t12, 11, 12

    section Plan 2 — bus-nats + bus-outbox
    Workspace deps               :p2t1, 12, 13
    Scaffold bus-nats            :p2t2, 13, 14
    NatsClient + ensure_stream   :p2t3, 14, 15
    NatsPublisher (TDD)          :p2t4, 15, 16
    Circuit breaker (TDD)        :p2t5, 16, 17
    NatsKvIdempotencyStore (TDD) :p2t6, 17, 18
    Pull consumer loop (TDD)     :p2t7, 18, 19
    Scaffold bus-outbox          :p2t8, 19, 20
    PostgresOutboxStore (TDD)    :p2t9, 20, 21
    PostgresIdempotencyStore     :p2t10, 21, 22
    SqliteBuffer (TDD)           :p2t11, 22, 23
    Full workspace check         :p2t12, 23, 24

    section Plan 3 — bus-telemetry + event-bus
    Workspace OTel deps          :p3t1, 24, 25
    Scaffold bus-telemetry       :p3t2, 25, 26
    W3C traceparent (TDD)        :p3t3, 26, 27
    OTel spans + metrics         :p3t4, 27, 28
    Scaffold event-bus           :p3t5, 28, 29
    EventBusBuilder + EventBus   :p3t6, 29, 30
    Saga engine (TDD)            :p3t7, 30, 31
    Example 01-basic-publish     :p3t8, 31, 32
    Example 03-idempotent-handler:p3t9, 32, 33
    Final workspace check        :p3t10, 33, 34
```

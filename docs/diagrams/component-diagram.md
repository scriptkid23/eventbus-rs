# eventbus-rs — Component Interaction Diagram

## C4 Level 3: Component Interactions (Full v1.0)

```mermaid
graph TB
    %% ── Application (library consumer) ──────────────────────────────
    subgraph app["Application"]
        APP_CODE["Application Code\n─────────────\npublish_in_tx()\nsubscribe()\nstart_saga()"]
        APP_TX["Postgres Transaction\n─────────────\nBusiness write +\noutbox insert (atomic)"]
    end

    %% ── event-bus facade ─────────────────────────────────────────────
    subgraph event_bus["event-bus — Public Facade"]
        EB_BUILDER["EventBusBuilder\n─────────────\nurl · idempotency (required)\noutbox · sqlite_buffer\nwith_otel"]
        EB_BUS["EventBus\n─────────────\npublish()\npublish_in_tx()\nsubscribe()\nshutdown()"]
        EB_SAGA["Saga Engine\n─────────────\nChoreographyStep\nSagaDefinition\nSagaHandle"]
        EB_PRELUDE["prelude.rs\n─────────────\nre-exports all\npublic API types"]
    end

    %% ── bus-core ─────────────────────────────────────────────────────
    subgraph bus_core["bus-core — Traits & Types"]
        BC_EVENT["Event trait\n─────────────\nsubject()\nmessage_id()\naggregate_type()"]
        BC_PUB["Publisher trait\n─────────────\npublish()\npublish_batch()"]
        BC_HANDLER["EventHandler trait\n─────────────\nhandle(ctx, event)"]
        BC_IDEM["IdempotencyStore trait\n─────────────\ntry_insert()\nmark_done()"]
        BC_MSGID["MessageId\n─────────────\nUUIDv7 newtype\nOrd · Hash · Serde"]
        BC_ERR["BusError / HandlerError\n─────────────\nTransient → NAK retry\nPermanent → Term DLQ"]
        BC_CTX["HandlerCtx\n─────────────\nmsg_id · stream_seq\ndelivered · span"]
        BC_RECEIPT["PubReceipt\n─────────────\nstream · sequence\nduplicate · buffered"]
    end

    %% ── bus-macros ───────────────────────────────────────────────────
    subgraph bus_macros["bus-macros — Proc Macro"]
        BM_DERIVE["#[derive(Event)]\n─────────────\nsubject template\nfield interpolation\ncompile-time validation"]
    end

    %% ── bus-nats ─────────────────────────────────────────────────────
    subgraph bus_nats["bus-nats — JetStream Backend"]
        BN_CLIENT["NatsClient\n─────────────\nconnect()\nensure_stream()"]
        BN_PUB["NatsPublisher\n─────────────\nimpl Publisher\nNats-Msg-Id header"]
        BN_SUB["subscriber.rs\n─────────────\npull consumer loop\nsemaphore concurrency"]
        BN_ACK["ack.rs\n─────────────\ndouble_ack()\nnak_with_delay()\nterm()"]
        BN_CB["CircuitBreaker\n─────────────\nsliding window\nClosed/Open/HalfOpen"]
        BN_DLQ["dlq.rs\n─────────────\nrepublish on\nMAX_DELIVERIES advisory"]
        BN_KV["NatsKvIdempotencyStore\n─────────────\nKV.Create (atomic)\nbucket: eventbus_processed"]
        BN_REDIS["RedisIdempotencyStore\n─────────────\nSET NX EX (atomic)\nfeature: redis-inbox"]
    end

    %% ── bus-outbox ───────────────────────────────────────────────────
    subgraph bus_outbox["bus-outbox — Outbox + Buffer"]
        BO_STORE["OutboxStore trait\n─────────────\ninsert() · fetch_pending()\nmark_published() · mark_failed()"]
        BO_PG["PostgresOutboxStore\n─────────────\nimpl OutboxStore\nfeature: postgres-outbox"]
        BO_DISP["OutboxDispatcher\n─────────────\npoll every 250ms\nSELECT FOR UPDATE\nSKIP LOCKED LIMIT 100"]
        BO_IDEM_PG["PostgresIdempotencyStore\n─────────────\nINSERT ON CONFLICT DO NOTHING\nfeature: postgres-inbox"]
        BO_SQLITE["SqliteBuffer\n─────────────\nNATS-down fallback\nrelay on recovery\nfeature: sqlite-buffer"]
        BO_MIGRATE["migrate.rs\n─────────────\noutbox · inbox\nsagas migrations"]
    end

    %% ── bus-telemetry ────────────────────────────────────────────────
    subgraph bus_telemetry["bus-telemetry — OTel (feature: otel)"]
        BT_INJECT["inject_context()\n─────────────\nW3C traceparent\n→ NATS header"]
        BT_EXTRACT["extract_context()\n─────────────\nNATS header\n→ parent span"]
        BT_SPANS["Auto spans\n─────────────\npublish · receive\nhandle · dispatch\nidempotency check"]
        BT_METRICS["Metrics\n─────────────\npublish.total\nconsume.total\noutbox.pending\ndlq.total"]
    end

    %% ── External Systems ─────────────────────────────────────────────
    subgraph ext["External Systems"]
        EXT_NATS[("NATS JetStream\nR3 · dedup 5min\nFileStorage")]
        EXT_PG[("PostgreSQL\neventbus_outbox\neventbus_inbox\neventbus_sagas")]
        EXT_SQLITE[("SQLite\neventbus_buffer\nlocal fallback")]
        EXT_OTEL[("OTel Collector\nJaeger / Prometheus\n/ OTLP")]
        EXT_REDIS[("Redis\nidempotency\nSET NX EX")]
    end

    %% ═══════════════════════════════════════════════════════════════
    %% COMPILE-TIME RELATIONSHIPS
    %% ═══════════════════════════════════════════════════════════════

    BM_DERIVE -.->|"generates impl"| BC_EVENT
    EB_BUILDER -->|"builds"| EB_BUS
    EB_BUS -->|"uses"| BC_PUB
    EB_BUS -->|"uses"| BC_IDEM
    EB_BUS -->|"uses"| BO_STORE
    EB_BUS -->|"uses"| EB_SAGA
    BN_PUB -.->|"impl"| BC_PUB
    BN_KV -.->|"impl"| BC_IDEM
    BN_REDIS -.->|"impl"| BC_IDEM
    BO_PG -.->|"impl"| BO_STORE
    BO_IDEM_PG -.->|"impl"| BC_IDEM

    %% ═══════════════════════════════════════════════════════════════
    %% RUNTIME: PUBLISH PATH (direct, no outbox)
    %% ═══════════════════════════════════════════════════════════════

    APP_CODE -->|"publish(event)"| EB_BUS
    EB_BUS -->|"publish(event)"| BN_PUB
    BN_PUB -->|"check"| BN_CB
    BN_CB -->|"Closed: allow"| EXT_NATS
    BN_CB -->|"Open: buffer"| BO_SQLITE
    BO_SQLITE -->|"relay on recovery"| EXT_NATS
    BN_PUB -->|"inject traceparent"| BT_INJECT
    BT_INJECT -->|"header"| EXT_NATS

    %% ═══════════════════════════════════════════════════════════════
    %% RUNTIME: PUBLISH PATH (transactional outbox)
    %% ═══════════════════════════════════════════════════════════════

    APP_CODE -->|"publish_in_tx(tx, event)"| EB_BUS
    APP_CODE -->|"BEGIN / business write"| APP_TX
    EB_BUS -->|"insert outbox row"| APP_TX
    APP_TX -->|"COMMIT"| EXT_PG
    BO_DISP -->|"SELECT FOR UPDATE SKIP LOCKED"| EXT_PG
    BO_DISP -->|"publish_with_headers"| BN_PUB
    BO_DISP -->|"mark_published"| BO_PG
    BO_PG -->|"UPDATE published_at"| EXT_PG

    %% ═══════════════════════════════════════════════════════════════
    %% RUNTIME: CONSUME PATH
    %% ═══════════════════════════════════════════════════════════════

    EXT_NATS -->|"fetch batch"| BN_SUB
    BN_SUB -->|"extract traceparent"| BT_EXTRACT
    BT_EXTRACT -->|"parent span"| BT_SPANS
    BN_SUB -->|"try_insert(msg_id)"| BC_IDEM
    BC_IDEM -->|"false: duplicate"| BN_ACK
    BC_IDEM -->|"true: new"| BN_SUB
    BN_SUB -->|"handle(ctx, event)"| BC_HANDLER
    BC_HANDLER -->|"Ok → mark_done"| BC_IDEM
    BC_HANDLER -->|"Ok → double_ack"| BN_ACK
    BC_HANDLER -->|"Transient → nak_with_delay"| BN_ACK
    BC_HANDLER -->|"Permanent → term"| BN_ACK
    BN_ACK -->|"Term advisory"| BN_DLQ
    BN_DLQ -->|"republish to DLQ"| EXT_NATS
    BN_SUB -->|"emit spans + metrics"| BT_METRICS

    %% ═══════════════════════════════════════════════════════════════
    %% RUNTIME: TELEMETRY EXPORT
    %% ═══════════════════════════════════════════════════════════════

    BT_SPANS -->|"export"| EXT_OTEL
    BT_METRICS -->|"export"| EXT_OTEL

    %% ═══════════════════════════════════════════════════════════════
    %% EXTERNAL BACKEND CONNECTIONS
    %% ═══════════════════════════════════════════════════════════════

    BN_KV -->|"KV.Create"| EXT_NATS
    BN_REDIS -->|"SET NX EX"| EXT_REDIS
    BO_IDEM_PG -->|"INSERT ON CONFLICT"| EXT_PG
    BO_SQLITE -->|"INSERT / DELETE"| EXT_SQLITE
    BO_MIGRATE -->|"run migrations"| EXT_PG

    %% ═══════════════════════════════════════════════════════════════
    %% STYLES
    %% ═══════════════════════════════════════════════════════════════

    classDef coreStyle fill:#dbeafe,stroke:#3b82f6,color:#1e3a5f
    classDef natsStyle fill:#dcfce7,stroke:#16a34a,color:#14532d
    classDef outboxStyle fill:#fef9c3,stroke:#ca8a04,color:#78350f
    classDef facadeStyle fill:#f3e8ff,stroke:#9333ea,color:#3b0764
    classDef macroStyle fill:#fce7f3,stroke:#db2777,color:#831843
    classDef telemStyle fill:#ffedd5,stroke:#ea580c,color:#7c2d12
    classDef extStyle fill:#f1f5f9,stroke:#64748b,color:#0f172a
    classDef appStyle fill:#ecfdf5,stroke:#059669,color:#064e3b

    class BC_EVENT,BC_PUB,BC_HANDLER,BC_IDEM,BC_MSGID,BC_ERR,BC_CTX,BC_RECEIPT coreStyle
    class BN_CLIENT,BN_PUB,BN_SUB,BN_ACK,BN_CB,BN_DLQ,BN_KV,BN_REDIS natsStyle
    class BO_STORE,BO_PG,BO_DISP,BO_IDEM_PG,BO_SQLITE,BO_MIGRATE outboxStyle
    class EB_BUILDER,EB_BUS,EB_SAGA,EB_PRELUDE facadeStyle
    class BM_DERIVE macroStyle
    class BT_INJECT,BT_EXTRACT,BT_SPANS,BT_METRICS telemStyle
    class EXT_NATS,EXT_PG,EXT_SQLITE,EXT_OTEL,EXT_REDIS extStyle
    class APP_CODE,APP_TX appStyle
```

---

## Edge Legend

| Edge style | Meaning |
|---|---|
| `-->` solid | Runtime interaction (data flows at execution time) |
| `-.->` dashed | Compile-time relationship (trait impl, code generation) |

## Color Legend

| Color | Crate |
|---|---|
| Blue | `bus-core` — traits & types |
| Green | `bus-nats` — JetStream backend |
| Yellow | `bus-outbox` — outbox + buffer |
| Purple | `event-bus` — public facade |
| Pink | `bus-macros` — proc macro |
| Orange | `bus-telemetry` — OTel |
| Gray | External systems |
| Teal | Application (library consumer) |

## Key Flows

### Publish (direct)
`App` → `EventBus` → `NatsPublisher` → `CircuitBreaker` → `NATS JetStream`  
If NATS down: `CircuitBreaker` → `SqliteBuffer` → relay on recovery → `NATS JetStream`

### Publish (transactional outbox)
`App` → `BEGIN tx` → business write + outbox insert → `COMMIT` → `PostgreSQL`  
`OutboxDispatcher` → `SELECT FOR UPDATE SKIP LOCKED` → `NatsPublisher` → `NATS JetStream`

### Consume
`NATS JetStream` → `subscriber` → idempotency check → `EventHandler`  
→ `Ok`: `mark_done` + `double_ack`  
→ `Transient`: `nak_with_delay` (backoff: 1s → 5s → 30s → 5m)  
→ `Permanent`: `term` → `DLQ` republish

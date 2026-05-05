# eventbus-rs — Component Interaction Diagram

## C4 Level 3: Component Interactions (Slim Transport Scope)

```mermaid
flowchart TB
    subgraph application [Application]
        appCode["Application code"]
    end

    subgraph eventBus [event-bus facade]
        builder["EventBusBuilder"]
        bus["EventBus"]
        prelude["prelude re-exports"]
    end

    subgraph busCore [bus-core traits and types]
        eventTrait["Event"]
        publisherTrait["Publisher"]
        handlerTrait["EventHandler"]
        idempotencyTrait["IdempotencyStore"]
    end

    subgraph busMacros [bus-macros]
        deriveEvent["derive(Event)"]
    end

    subgraph busNats [bus-nats backend]
        natsClient["NatsClient"]
        natsPublisher["NatsPublisher"]
        subscriber["subscriber"]
        circuitBreaker["CircuitBreaker"]
        dlq["DLQ helpers"]
        kvStore["NatsKvIdempotencyStore"]
        redisStore["RedisIdempotencyStore"]
        sqliteBuffer["SqliteBuffer"]
    end

    subgraph telemetry [bus-telemetry optional]
        inject["inject_context"]
        extract["extract_context"]
        metrics["publish and consume metrics"]
    end

    subgraph external [External systems]
        jetstream[("NATS JetStream")]
        redis[("Redis")]
        sqlite[("SQLite local buffer")]
        otel[("OTel collector")]
    end

    deriveEvent -.-> eventTrait
    builder --> bus
    bus --> publisherTrait
    bus --> idempotencyTrait
    natsPublisher -.-> publisherTrait
    kvStore -.-> idempotencyTrait
    redisStore -.-> idempotencyTrait

    appCode -->|"publish and subscribe"| bus
    bus --> natsPublisher
    natsPublisher --> circuitBreaker
    circuitBreaker -->|"healthy"| jetstream
    circuitBreaker -->|"unavailable"| sqliteBuffer
    sqliteBuffer -->|"replay on recovery"| jetstream

    jetstream --> subscriber
    subscriber --> handlerTrait
    subscriber --> dlq
    dlq --> jetstream
    kvStore --> jetstream
    redisStore --> redis
    sqliteBuffer --> sqlite

    natsPublisher --> inject
    subscriber --> extract
    extract --> metrics
    metrics --> otel
```

## Key Flows

### Publish
`Application` -> `EventBus` -> `NatsPublisher` -> `CircuitBreaker` -> `NATS JetStream`

If NATS is unavailable: `CircuitBreaker` -> `SqliteBuffer` -> replay to JetStream on recovery.

### Consume
`NATS JetStream` -> `subscriber` -> `IdempotencyStore` -> `EventHandler` -> ACK/NAK/Term -> optional DLQ publish.

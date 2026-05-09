# eventbus-rs — Component Interaction Diagram

## C4 Level 3: Component Interactions (Slim Transport Scope)

```mermaid
flowchart TB
    subgraph application [Application]
        appCode["Application code"]
    end

    subgraph eventBus [eventbus-nats façade]
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

    subgraph busMacros [eventbus-macros]
        deriveEvent["derive(Event)"]
    end

    subgraph busNats [bus-nats backend]
        natsClient["NatsClient"]
        natsPublisher["NatsPublisher"]
        subscriber["subscriber"]
        dlq["DLQ helpers"]
        kvStore["NatsKvIdempotencyStore"]
        redisStore["RedisIdempotencyStore"]
    end

    subgraph external [External systems]
        jetstream[("NATS JetStream")]
        redis[("Redis")]
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
    natsPublisher --> jetstream

    jetstream --> subscriber
    subscriber --> handlerTrait
    subscriber --> dlq
    dlq --> jetstream
    kvStore --> jetstream
    redisStore --> redis
```

## Key Flows

### Publish
`Application` -> `EventBus` -> `NatsPublisher` -> `NATS JetStream`

### Consume
`NATS JetStream` -> `subscriber` -> `IdempotencyStore` -> `EventHandler` -> ACK/NAK/Term -> optional DLQ publish.

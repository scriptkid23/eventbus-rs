# eventbus-rs — System Diagrams (Slim Transport Scope)

## 1. Workspace Dependency Graph

```mermaid
flowchart TD
    subgraph workspace [Cargo workspace]
        busCore[bus-core]
        busMacros[bus-macros]
        busNats[bus-nats]
        busTelemetry[bus-telemetry]
        eventBus[event-bus]
    end

    busMacros --> busCore
    busNats --> busCore
    busTelemetry --> busCore
    eventBus --> busCore
    eventBus --> busNats
    eventBus -->|"optional feature: otel"| busTelemetry
    eventBus -->|"optional feature: macros"| busMacros
```

## 2. event-bus Feature Map

```mermaid
flowchart LR
    eventBus[event-bus]
    macros["macros -> bus-macros"]
    natsKv["nats-kv-inbox -> bus-nats nats_kv"]
    redisInbox["redis-inbox -> bus-nats redis"]
    sqliteBuffer["sqlite-buffer -> bus-nats sqlite_buffer"]
    otel["otel -> bus-telemetry"]

    eventBus --> macros
    eventBus --> natsKv
    eventBus --> redisInbox
    eventBus --> sqliteBuffer
    eventBus --> otel
```

## 3. Publish and Consume Flow

```mermaid
flowchart TD
    app[Application]
    builder[EventBusBuilder]
    bus[EventBus]
    publisher[NatsPublisher]
    cb[CircuitBreaker]
    js[JetStream]
    sqlite[SqliteBuffer]
    sub[Subscriber]
    idem[IdempotencyStore]
    handler[EventHandler]
    dlq[DLQ stream]

    app --> builder --> bus
    app -->|"publish(event)"| bus
    bus --> publisher --> cb
    cb -->|"healthy"| js
    cb -->|"nats unavailable"| sqlite
    sqlite -->|"relay when healthy"| js

    js -->|"deliver message"| sub
    sub --> idem
    idem -->|"new claim"| handler
    idem -->|"duplicate"| sub
    handler -->|"ok/transient/permanent"| sub
    sub --> dlq
```

## 4. Telemetry Propagation

```mermaid
sequenceDiagram
    participant svcA as PublisherService
    participant nats as NATSJetStream
    participant svcB as ConsumerService
    participant otel as OTelCollector

    svcA->>nats: publish with traceparent header
    nats-->>svcB: deliver message with headers
    svcB->>svcB: extract parent context and create child span
    svcA->>otel: export spans and metrics
    svcB->>otel: export spans and metrics
```

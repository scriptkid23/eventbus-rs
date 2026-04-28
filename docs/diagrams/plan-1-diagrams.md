# Plan 1: bus-core + bus-macros — Diagrams

## 1. Workspace Crate Dependency Graph

```mermaid
graph TD
    subgraph Workspace["Cargo Workspace"]
        BC["bus-core<br/>(leaf — no workspace deps)"]
        BM["bus-macros<br/>(proc-macro = true)"]
    end

    BM -->|dev-dep| BC

    subgraph ExtDeps["External Dependencies"]
        AT["async-trait 0.1"]
        SE["serde 1"]
        SJ["serde_json 1"]
        TE["thiserror 1"]
        TR["tracing 0.1"]
        UU["uuid 1 (v7 + serde)"]
        PM["proc-macro2 1"]
        QU["quote 1"]
        SY["syn 2 (full)"]
    end

    BC --> AT
    BC --> SE
    BC --> SJ
    BC --> TE
    BC --> TR
    BC --> UU

    BM --> PM
    BM --> QU
    BM --> SY
```

---

## 2. bus-core Module Structure

```mermaid
graph LR
    lib["lib.rs<br/>(pub re-exports)"]

    lib --> id["id.rs<br/>MessageId"]
    lib --> error["error.rs<br/>BusError · HandlerError"]
    lib --> event["event.rs<br/>Event trait"]
    lib --> publisher["publisher.rs<br/>Publisher · PubReceipt"]
    lib --> handler["handler.rs<br/>EventHandler · HandlerCtx"]
    lib --> idempotency["idempotency.rs<br/>IdempotencyStore"]

    event -->|uses| id
    publisher -->|uses| event
    publisher -->|uses| error
    handler -->|uses| event
    handler -->|uses| error
    handler -->|uses| id
    idempotency -->|uses| error
    idempotency -->|uses| id
```

---

## 3. Component Diagram — bus-core Types & Traits

```mermaid
classDiagram
    class MessageId {
        +Uuid inner
        +new() MessageId
        +from_uuid(u: Uuid) MessageId
        +as_uuid() &Uuid
        +Display
        +FromStr
        +PartialOrd / Ord
        +Serialize / Deserialize
    }

    class Event {
        <<trait>>
        +subject(&self) Cow~str~
        +message_id(&self) MessageId
        +aggregate_type() &'static str
        --
        requires: Serialize + DeserializeOwned
        requires: Send + Sync + 'static
    }

    class PubReceipt {
        +stream: String
        +sequence: u64
        +duplicate: bool
        +buffered: bool
    }

    class Publisher {
        <<trait>>
        +publish(E: Event) Result~PubReceipt, BusError~
        +publish_batch(events: &[E]) Result~Vec~PubReceipt~, BusError~
        --
        requires: Send + Sync
    }

    class HandlerCtx {
        +msg_id: MessageId
        +stream_seq: u64
        +delivered: u64
        +subject: String
        +span: Span
    }

    class EventHandler {
        <<trait>>
        +handle(ctx: HandlerCtx, event: E) Result~(), HandlerError~
        --
        requires: Send + Sync + 'static
    }

    class IdempotencyStore {
        <<trait>>
        +try_insert(key: &MessageId, ttl: Duration) Result~bool, BusError~
        +mark_done(key: &MessageId) Result~(), BusError~
        --
        requires: Send + Sync
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
        Transient(String)
        Permanent(String)
    }

    Event --> MessageId : message_id() returns
    Publisher --> Event : generic over E
    Publisher --> PubReceipt : returns
    Publisher --> BusError : returns on error
    EventHandler --> Event : generic over E
    EventHandler --> HandlerCtx : receives
    EventHandler --> HandlerError : returns on error
    IdempotencyStore --> MessageId : keyed by
    IdempotencyStore --> BusError : returns on error
    BusError --> HandlerError : wraps via From
```

---

## 4. bus-macros — #[derive(Event)] Code Generation Flow

```mermaid
flowchart TD
    A["User writes:<br/>#[derive(Event)]<br/>#[event(subject = &quot;orders.{self.order_id}.created&quot;, aggregate = &quot;order&quot;)]<br/>struct OrderCreated &#123; id: MessageId, order_id: Uuid &#125;"]

    A --> B["proc_macro_derive<br/>derive_event(input: TokenStream)"]
    B --> C["syn::parse_macro_input!<br/>→ DeriveInput AST"]
    C --> D{"Has #[event(...)]<br/>attribute?"}
    D -->|no| E["compile_error!<br/>missing #[event(subject = ...)]"]
    D -->|yes| F["parse subject = &quot;...&quot;<br/>and aggregate = &quot;...&quot;"]
    F --> G{"Has named field<br/>id: MessageId?"}
    G -->|no| H["compile_error!<br/>requires field named `id`"]
    G -->|yes| I["build_subject_expr()<br/>parse {self.field} tokens"]
    I --> J{"Any unclosed &#123;<br/>or invalid expr?"}
    J -->|yes| K["compile_error!<br/>invalid template"]
    J -->|no| L["quote! generate<br/>impl bus_core::Event for OrderCreated"]
    L --> M["subject() → format!(\"orders.{}.created\", self.order_id)"]
    L --> N["message_id() → self.id.clone()"]
    L --> O["aggregate_type() → \"order\""]
```

---

## 5. Task Execution Sequence (Plan 1)

```mermaid
sequenceDiagram
    participant W as Workspace Cargo.toml
    participant BC as bus-core
    participant BM as bus-macros
    participant T as Tests

    Note over W: Task 1 — workspace manifest
    W->>W: add [workspace] + members + [workspace.dependencies]

    Note over BC: Task 2 — scaffold
    BC->>BC: Cargo.toml + src/lib.rs (empty mods)

    Note over BC: Task 3 — MessageId
    T->>BC: id_test.rs (failing)
    BC->>BC: src/id.rs
    T->>BC: cargo test id_test ✓

    Note over BC: Task 4 — errors
    T->>BC: error_test.rs (failing)
    BC->>BC: src/error.rs
    T->>BC: cargo test error_test ✓

    Note over BC: Tasks 5–8 — traits
    BC->>BC: event.rs / publisher.rs / handler.rs / idempotency.rs
    BC->>BC: cargo check ✓

    Note over BM: Task 9 — scaffold
    BM->>BM: Cargo.toml + src/lib.rs + src/event.rs (stub)

    Note over BM: Task 10 — derive impl
    T->>BM: derive_test.rs (failing)
    BM->>BM: src/event.rs (full impl)
    T->>BM: cargo test derive_test ✓

    Note over BM: Task 11 — compile-fail tests
    BM->>BM: trybuild fixtures + .stderr snapshots

    Note over W,T: Task 12 — final check
    T->>W: cargo test --workspace ✓
    T->>W: cargo clippy --workspace ✓
```

---

## 6. Subject Template Parsing Example

```mermaid
flowchart LR
    IN["&quot;orders.{self.order_id}.shipped&quot;"]

    IN --> S1["literal: &quot;orders.&quot;"]
    IN --> P1["{self.order_id}"]
    IN --> S2["literal: &quot;.shipped&quot;"]

    P1 --> FI["field ident: order_id"]

    S1 --> FS["format_str:<br/>&quot;orders.{}.shipped&quot;"]
    S2 --> FS
    FI --> ARGS["args: [self.order_id]"]

    FS --> OUT["format!(&quot;orders.{}.shipped&quot;, self.order_id)"]
    ARGS --> OUT
```

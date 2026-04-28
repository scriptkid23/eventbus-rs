# eventbus-rs

Foundational Rust crates for an async **event bus** abstraction: **`bus-core`** (trait-only, no NATS/runtime lock-in) and **`bus-macros`** (`#[derive(Event)]` with compile-time subject validation).

**License:** MIT OR Apache-2.0  
**Repository:** [github.com/1hoodlabs/eventbus-rs](https://github.com/1hoodlabs/eventbus-rs)

---

## Overview

This workspace is the **core layer** of a larger event-bus stack. Today it provides:

- Strongly typed **`Event`** plus **`Publisher`**, **`EventHandler`**, and **`IdempotencyStore`** traits
- **`MessageId`**: UUIDv7 newtype (monotonic, serialization-friendly)
- **`BusError`** / **`HandlerError`**: structured errors for transports and handlers
- **`#[derive(Event)]`**: `subject` templates with `{self.field}` interpolation and `aggregate` metadata

Transports (for example NATS JetStream), outbox, and concrete stores are intentionally **out of scope** for these crates so `bus-core` stays dependency-light.

---

## Requirements

- **Rust toolchain:** **1.85.0** or newer (workspace uses **edition 2024**)

---

## Workspace layout

| Crate        | Purpose |
|-------------|---------|
| [`bus-core`](crates/bus-core/)   | Traits and types shared by publishers and consumers (no NATS-specific deps) |
| [`bus-macros`](crates/bus-macros/) | Proc-macro `#[derive(Event)]` and `#[event(...)]` attributes |

---

## Quick start

Add the crates to your `Cargo.toml` (paths shown for local development; published versions will use `version = "..."` from [crates.io](https://crates.io/) when available):

```toml
[dependencies]
bus-core = { path = "crates/bus-core" }
bus-macros = { path = "crates/bus-macros" }
serde = { version = "1", features = ["derive"] }
uuid = { version = "1", features = ["v7"] }
```

Minimal usage:

```rust
use bus_core::{Event, MessageId};
use bus_macros::Event;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Event)]
#[event(subject = "orders.{self.order_id}.created")]
struct OrderCreated {
    id: MessageId,
    order_id: uuid::Uuid,
}

let event = OrderCreated {
    id: MessageId::new(),
    order_id: uuid::Uuid::now_v7(),
};

assert!(event.subject().contains("orders."));
```

The derive macro requires:

- `#[event(subject = "...")]` on the struct
- A field named `id` with type `bus_core::MessageId`

See integration tests under [`crates/bus-macros/tests/`](crates/bus-macros/tests/) for more examples and compile-fail coverage ([`trybuild`](https://github.com/dtolnay/trybuild)).

---

## Development

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

---

## Contributing

Contributions are welcome.

1. **Issues first** for sizeable changes (API, new crates, behavior).
2. Keep **`bus-core`** free of transport-specific dependencies unless the project explicitly expands scope.
3. Match existing style; add or update tests when behavior changes (including `trybuild` snapshots under `crates/bus-macros/tests/compile_fail/` when diagnostics change).

---

## Security

If you discover a security issue, please **do not** file a public issue. Contact the maintainers privately (see repository contact options on GitHub) with steps to reproduce and impact.

---

## License

Licensed under either of

- Apache License, Version 2.0 ([`LICENSE`](LICENSE) or <https://www.apache.org/licenses/LICENSE-2.0>)
- MIT license, at your option

Rust ecosystem convention for dual licensing is documented in the [Rust RFC](https://rust-lang.github.io/rfcs/2582-license-MITNAPACHE.html).  
If you add a `LICENSE-MIT` file alongside the Apache `LICENSE`, link it here for completeness.

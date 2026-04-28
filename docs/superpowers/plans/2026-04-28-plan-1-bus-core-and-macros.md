# Plan 1: `bus-core` + `bus-macros` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the foundational crates — zero-dep trait definitions (`bus-core`) and the `#[derive(Event)]` proc-macro (`bus-macros`) — that all other crates depend on.

**Architecture:** `bus-core` is the leaf crate with no workspace deps, defining `Event`, `Publisher`, `EventHandler`, `IdempotencyStore` traits, `MessageId` (UUIDv7 newtype), `PubReceipt`, `HandlerCtx`, `HandlerError`, and `BusError`. `bus-macros` is a separate `proc-macro = true` crate that generates `impl Event` from `#[derive(Event)]` with compile-time subject template validation.

**Tech Stack:** Rust 2024 edition, `uuid` (v7), `serde`, `thiserror`, `async-trait`, `syn` 2.x, `quote`, `proc-macro2`

---

## File Map

### Workspace root
- Modify: `Cargo.toml` — add `[workspace]` with all crate members

### `crates/bus-core/`
- Create: `crates/bus-core/Cargo.toml`
- Create: `crates/bus-core/src/lib.rs` — pub mod declarations
- Create: `crates/bus-core/src/id.rs` — `MessageId` newtype over UUIDv7
- Create: `crates/bus-core/src/error.rs` — `BusError`, `HandlerError`
- Create: `crates/bus-core/src/event.rs` — `Event` trait
- Create: `crates/bus-core/src/publisher.rs` — `Publisher` trait, `PubReceipt`
- Create: `crates/bus-core/src/handler.rs` — `EventHandler` trait, `HandlerCtx`
- Create: `crates/bus-core/src/idempotency.rs` — `IdempotencyStore` trait
- Create: `crates/bus-core/tests/id_test.rs` — `MessageId` unit tests
- Create: `crates/bus-core/tests/error_test.rs` — `BusError` unit tests

### `crates/bus-macros/`
- Create: `crates/bus-macros/Cargo.toml`
- Create: `crates/bus-macros/src/lib.rs` — proc-macro entry point
- Create: `crates/bus-macros/src/event.rs` — derive logic, subject template parsing
- Create: `crates/bus-macros/tests/derive_test.rs` — compile-pass + compile-fail tests

---

## Task 1: Convert root `Cargo.toml` to workspace

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Replace root Cargo.toml with workspace manifest**

```toml
[workspace]
members = [
    "crates/bus-core",
    "crates/bus-macros",
]
resolver = "2"

[workspace.package]
version    = "0.1.0"
edition    = "2024"
authors    = ["Olivier Taylor <tech@mey.network>"]
license    = "MIT OR Apache-2.0"
repository = "https://github.com/1hoodlabs/eventbus-rs"

[workspace.dependencies]
async-trait  = "0.1"
bytes        = "1.5"
serde        = { version = "1", features = ["derive"] }
serde_json   = "1"
thiserror    = "1"
tokio        = { version = "1.36", features = ["full"] }
tracing      = "0.1"
uuid         = { version = "1", features = ["v7", "serde"] }
# proc-macro deps
proc-macro2  = "1"
quote        = "1"
syn          = { version = "2", features = ["full"] }
```

- [ ] **Step 2: Verify workspace parses**

```bash
cargo metadata --no-deps --format-version 1 | head -5
```

Expected: JSON output starting with `{"workspace_root":` — no errors.

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml
git commit -m "chore: convert to cargo workspace, add bus-core and bus-macros members"
```

---

## Task 2: Scaffold `bus-core` crate

**Files:**
- Create: `crates/bus-core/Cargo.toml`
- Create: `crates/bus-core/src/lib.rs`

- [ ] **Step 1: Create directory structure**

```bash
mkdir -p crates/bus-core/src
```

- [ ] **Step 2: Create `crates/bus-core/Cargo.toml`**

```toml
[package]
name        = "bus-core"
description = "Core traits and types for eventbus-rs — zero NATS dependencies"
keywords    = ["nats", "eventbus", "messaging", "async"]
categories  = ["asynchronous", "network-programming"]
version.workspace    = true
edition.workspace    = true
authors.workspace    = true
license.workspace    = true
repository.workspace = true

[dependencies]
async-trait.workspace = true
serde       = { workspace = true }
serde_json  = { workspace = true }
thiserror   = { workspace = true }
tracing     = { workspace = true }
uuid        = { workspace = true }
```

- [ ] **Step 3: Create `crates/bus-core/src/lib.rs`**

```rust
pub mod error;
pub mod event;
pub mod handler;
pub mod id;
pub mod idempotency;
pub mod publisher;

pub use error::{BusError, HandlerError};
pub use event::Event;
pub use handler::{EventHandler, HandlerCtx};
pub use id::MessageId;
pub use idempotency::IdempotencyStore;
pub use publisher::{PubReceipt, Publisher};
```

- [ ] **Step 4: Verify it compiles (empty modules OK for now)**

```bash
cargo check -p bus-core
```

Expected: `Finished` with no errors (modules exist but are empty).

---

## Task 3: Implement `MessageId`

**Files:**
- Create: `crates/bus-core/src/id.rs`
- Create: `crates/bus-core/tests/id_test.rs`

- [ ] **Step 1: Write failing tests first**

Create `crates/bus-core/tests/id_test.rs`:

```rust
use bus_core::MessageId;
use std::str::FromStr;

#[test]
fn new_ids_are_unique() {
    let a = MessageId::new();
    let b = MessageId::new();
    assert_ne!(a, b);
}

#[test]
fn id_roundtrips_through_string() {
    let id = MessageId::new();
    let s = id.to_string();
    let id2 = MessageId::from_str(&s).unwrap();
    assert_eq!(id, id2);
}

#[test]
fn ids_are_monotonically_increasing() {
    let ids: Vec<MessageId> = (0..100).map(|_| MessageId::new()).collect();
    // UUIDv7 is time-ordered, string comparison works
    let strings: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
    let mut sorted = strings.clone();
    sorted.sort();
    assert_eq!(strings, sorted, "MessageIds must be monotonically increasing");
}

#[test]
fn from_str_rejects_invalid_uuid() {
    let result = "not-a-uuid".parse::<MessageId>();
    assert!(result.is_err());
}
```

- [ ] **Step 2: Run tests to confirm they fail**

```bash
cargo test -p bus-core --test id_test 2>&1 | head -20
```

Expected: compile error — `MessageId` not yet defined in `id.rs`.

- [ ] **Step 3: Implement `crates/bus-core/src/id.rs`**

```rust
use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};
use uuid::Uuid;

/// Newtype wrapper over UUIDv7 — monotonic, B-tree friendly, used as Nats-Msg-Id
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MessageId(Uuid);

impl MessageId {
    /// Generate a new UUIDv7-based message ID
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    /// Wrap an existing UUID
    pub fn from_uuid(u: Uuid) -> Self {
        Self(u)
    }

    /// Return the inner UUID
    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl Default for MessageId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for MessageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for MessageId {
    type Err = uuid::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(Uuid::parse_str(s)?))
    }
}
```

- [ ] **Step 4: Run tests to confirm they pass**

```bash
cargo test -p bus-core --test id_test
```

Expected:
```
test ids_are_monotonically_increasing ... ok
test new_ids_are_unique ... ok
test id_roundtrips_through_string ... ok
test from_str_rejects_invalid_uuid ... ok
```

- [ ] **Step 5: Commit**

```bash
git add crates/bus-core/
git commit -m "feat(bus-core): add MessageId UUIDv7 newtype with tests"
```

---

## Task 4: Implement `BusError` and `HandlerError`

**Files:**
- Create: `crates/bus-core/src/error.rs`
- Create: `crates/bus-core/tests/error_test.rs`

- [ ] **Step 1: Write failing tests**

Create `crates/bus-core/tests/error_test.rs`:

```rust
use bus_core::{BusError, HandlerError};

#[test]
fn handler_error_display_transient() {
    let e = HandlerError::Transient("db timeout".into());
    assert_eq!(e.to_string(), "transient: db timeout");
}

#[test]
fn handler_error_display_permanent() {
    let e = HandlerError::Permanent("invalid payload".into());
    assert_eq!(e.to_string(), "permanent: invalid payload");
}

#[test]
fn bus_error_from_handler_error() {
    let he = HandlerError::Transient("x".into());
    let be: BusError = he.into();
    assert!(matches!(be, BusError::Handler(_)));
}

#[test]
fn bus_error_nats_unavailable_display() {
    let e = BusError::NatsUnavailable;
    assert_eq!(e.to_string(), "nats unavailable");
}

#[test]
fn bus_error_from_serde_json() {
    let json_err = serde_json::from_str::<serde_json::Value>("not json").unwrap_err();
    let be: BusError = json_err.into();
    assert!(matches!(be, BusError::Serde(_)));
}
```

- [ ] **Step 2: Run tests to confirm they fail**

```bash
cargo test -p bus-core --test error_test 2>&1 | head -20
```

Expected: compile error — types not defined yet.

- [ ] **Step 3: Implement `crates/bus-core/src/error.rs`**

```rust
use thiserror::Error;

/// Errors returned by handler implementations
#[derive(Debug, Error)]
pub enum HandlerError {
    /// Transient failure — message will be NAK'd and retried with backoff
    #[error("transient: {0}")]
    Transient(String),

    /// Permanent failure — message will be Term'd and sent to DLQ
    #[error("permanent: {0}")]
    Permanent(String),
}

/// Top-level error type for all eventbus-rs operations
#[derive(Debug, Error)]
pub enum BusError {
    #[error("nats: {0}")]
    Nats(String),

    #[error("publish: {0}")]
    Publish(String),

    #[error("outbox: {0}")]
    Outbox(String),

    #[error("idempotency: {0}")]
    Idempotency(String),

    #[error("serialization: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("handler: {0}")]
    Handler(#[from] HandlerError),

    #[error("nats unavailable")]
    NatsUnavailable,
}
```

- [ ] **Step 4: Run tests to confirm they pass**

```bash
cargo test -p bus-core --test error_test
```

Expected: all 5 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/bus-core/src/error.rs crates/bus-core/tests/error_test.rs
git commit -m "feat(bus-core): add BusError and HandlerError with tests"
```

---

## Task 5: Implement `Event` trait

**Files:**
- Create: `crates/bus-core/src/event.rs`

- [ ] **Step 1: Implement `crates/bus-core/src/event.rs`**

No separate test file needed — this is a trait with no logic. It is tested indirectly via `bus-macros` derive tests in Task 9.

```rust
use crate::id::MessageId;
use serde::{de::DeserializeOwned, Serialize};
use std::borrow::Cow;

/// Marker trait for all events published through the event bus.
///
/// Implement manually or use `#[derive(Event)]` from the `bus-macros` crate.
/// Every implementor must have a stable, unique `message_id()` for idempotency
/// and deduplication.
pub trait Event: Serialize + DeserializeOwned + Send + Sync + 'static {
    /// NATS subject for this event, e.g. `"orders.created"`
    fn subject(&self) -> Cow<'_, str>;

    /// Unique identifier used as `Nats-Msg-Id` header and idempotency store key.
    /// Must be stable across retries — generate once at construction time.
    fn message_id(&self) -> MessageId;

    /// Aggregate type used for outbox routing. Defaults to `"default"`.
    fn aggregate_type() -> &'static str
    where
        Self: Sized,
    {
        "default"
    }
}
```

- [ ] **Step 2: Verify compile**

```bash
cargo check -p bus-core
```

Expected: `Finished` with no errors.

- [ ] **Step 3: Commit**

```bash
git add crates/bus-core/src/event.rs
git commit -m "feat(bus-core): add Event trait"
```

---

## Task 6: Implement `Publisher` trait and `PubReceipt`

**Files:**
- Create: `crates/bus-core/src/publisher.rs`

- [ ] **Step 1: Implement `crates/bus-core/src/publisher.rs`**

```rust
use crate::{error::BusError, event::Event};
use async_trait::async_trait;

/// Receipt returned after a successful publish
#[derive(Debug, Clone)]
pub struct PubReceipt {
    /// Name of the JetStream stream that received the message
    pub stream: String,

    /// JetStream sequence number assigned to this message
    pub sequence: u64,

    /// True if JetStream dedup suppressed this publish (same Nats-Msg-Id already stored).
    /// Do NOT count duplicate=true receipts as successful new publishes.
    pub duplicate: bool,

    /// True if the message was stored in the local SQLite buffer instead of NATS.
    /// The background relay task will forward it when NATS becomes reachable.
    pub buffered: bool,
}

/// Publishes events to the event bus
#[async_trait]
pub trait Publisher: Send + Sync {
    /// Publish a single event. Attaches `Nats-Msg-Id` header from `event.message_id()`.
    async fn publish<E: Event>(&self, event: &E) -> Result<PubReceipt, BusError>;

    /// Publish multiple events. Default implementation is sequential.
    /// Backends may override with batched async publish for higher throughput.
    async fn publish_batch<E: Event>(
        &self,
        events: &[E],
    ) -> Result<Vec<PubReceipt>, BusError> {
        let mut receipts = Vec::with_capacity(events.len());
        for event in events {
            receipts.push(self.publish(event).await?);
        }
        Ok(receipts)
    }
}
```

- [ ] **Step 2: Verify compile**

```bash
cargo check -p bus-core
```

Expected: `Finished` with no errors.

- [ ] **Step 3: Commit**

```bash
git add crates/bus-core/src/publisher.rs
git commit -m "feat(bus-core): add Publisher trait and PubReceipt"
```

---

## Task 7: Implement `EventHandler` trait and `HandlerCtx`

**Files:**
- Create: `crates/bus-core/src/handler.rs`

- [ ] **Step 1: Implement `crates/bus-core/src/handler.rs`**

```rust
use crate::{error::HandlerError, event::Event, id::MessageId};
use async_trait::async_trait;
use tracing::Span;

/// Context passed to every handler invocation
#[derive(Debug)]
pub struct HandlerCtx {
    /// The message ID (same as Nats-Msg-Id header)
    pub msg_id: MessageId,

    /// JetStream stream sequence number for this message
    pub stream_seq: u64,

    /// Number of times this message has been delivered (1 = first delivery)
    pub delivered: u64,

    /// NATS subject this message was received on
    pub subject: String,

    /// Active tracing span — use `span.in_scope(|| ...)` or `Instrument` to attach work
    pub span: Span,
}

/// Processes a strongly-typed event received from the event bus.
///
/// Return `Err(HandlerError::Transient(_))` to NAK and retry with backoff.
/// Return `Err(HandlerError::Permanent(_))` to Term and send to DLQ.
#[async_trait]
pub trait EventHandler<E: Event>: Send + Sync + 'static {
    async fn handle(&self, ctx: HandlerCtx, event: E) -> Result<(), HandlerError>;
}
```

- [ ] **Step 2: Verify compile**

```bash
cargo check -p bus-core
```

Expected: `Finished` with no errors.

- [ ] **Step 3: Commit**

```bash
git add crates/bus-core/src/handler.rs
git commit -m "feat(bus-core): add EventHandler trait and HandlerCtx"
```

---

## Task 8: Implement `IdempotencyStore` trait

**Files:**
- Create: `crates/bus-core/src/idempotency.rs`

- [ ] **Step 1: Implement `crates/bus-core/src/idempotency.rs`**

```rust
use crate::{error::BusError, id::MessageId};
use async_trait::async_trait;
use std::time::Duration;

/// Backend-agnostic idempotency store for deduplicating event handler executions.
///
/// There is no default implementation — callers must explicitly choose a backend
/// (NATS KV, PostgreSQL, or Redis) to avoid hidden dependencies.
///
/// All implementations MUST use atomic operations (e.g. `INSERT ... ON CONFLICT DO NOTHING`,
/// `KV.Create`, `SET NX`) to be safe under concurrent delivery.
#[async_trait]
pub trait IdempotencyStore: Send + Sync {
    /// Attempt to claim this message ID for processing.
    ///
    /// Returns `Ok(true)` if this is the first time this key has been seen —
    /// the caller should proceed with business logic.
    ///
    /// Returns `Ok(false)` if the key already exists — the caller should
    /// skip processing and double-ack the message.
    ///
    /// The key expires after `ttl`. TTL must be greater than the sum of
    /// max producer retry window + max consumer redelivery window + clock skew.
    async fn try_insert(&self, key: &MessageId, ttl: Duration) -> Result<bool, BusError>;

    /// Mark a previously inserted key as successfully processed.
    /// Called after business logic commits successfully.
    async fn mark_done(&self, key: &MessageId) -> Result<(), BusError>;
}
```

- [ ] **Step 2: Verify full `bus-core` compiles cleanly**

```bash
cargo check -p bus-core
cargo test -p bus-core
```

Expected: `Finished` and all existing tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/bus-core/src/idempotency.rs
git commit -m "feat(bus-core): add IdempotencyStore trait"
```

---

## Task 9: Scaffold `bus-macros` crate

**Files:**
- Create: `crates/bus-macros/Cargo.toml`
- Create: `crates/bus-macros/src/lib.rs`
- Create: `crates/bus-macros/src/event.rs`
- Modify: `Cargo.toml` — add `crates/bus-macros` to workspace members (already added in Task 1)

- [ ] **Step 1: Create directory structure**

```bash
mkdir -p crates/bus-macros/src
```

- [ ] **Step 2: Create `crates/bus-macros/Cargo.toml`**

```toml
[package]
name        = "bus-macros"
description = "Derive macros for eventbus-rs — #[derive(Event)]"
keywords    = ["nats", "eventbus", "macros", "derive"]
categories  = ["asynchronous", "network-programming"]
version.workspace    = true
edition.workspace    = true
authors.workspace    = true
license.workspace    = true
repository.workspace = true

[lib]
proc-macro = true

[dependencies]
proc-macro2 = { workspace = true }
quote       = { workspace = true }
syn         = { workspace = true }

[dev-dependencies]
bus-core = { path = "../bus-core" }
serde    = { workspace = true, features = ["derive"] }
uuid     = { workspace = true }
```

- [ ] **Step 3: Create `crates/bus-macros/src/lib.rs`**

```rust
mod event;

use proc_macro::TokenStream;

/// Derive macro that generates an `impl bus_core::Event` for a struct.
///
/// # Required
/// - The struct must have a field `id: bus_core::MessageId`
/// - The `#[event(subject = "...")]` attribute must be present
///
/// # Subject templates
/// Use `{self.field_name}` to interpolate struct fields into the subject.
/// Example: `#[event(subject = "orders.{self.order_id}.created")]`
///
/// # Optional attributes
/// - `aggregate = "order"` — sets `aggregate_type()`, defaults to `"default"`
#[proc_macro_derive(Event, attributes(event))]
pub fn derive_event(input: TokenStream) -> TokenStream {
    event::derive_event_impl(input)
}
```

- [ ] **Step 4: Create `crates/bus-macros/src/event.rs`** (stub — full impl in Task 10)

```rust
use proc_macro::TokenStream;

pub fn derive_event_impl(input: TokenStream) -> TokenStream {
    // Placeholder — implemented in Task 10
    let _ = input;
    TokenStream::new()
}
```

- [ ] **Step 5: Verify scaffold compiles**

```bash
cargo check -p bus-macros
```

Expected: `Finished` with no errors.

- [ ] **Step 6: Commit**

```bash
git add crates/bus-macros/
git commit -m "feat(bus-macros): scaffold proc-macro crate"
```

---

## Task 10: Implement `#[derive(Event)]` — subject parsing and code generation

**Files:**
- Modify: `crates/bus-macros/src/event.rs`

- [ ] **Step 1: Write compile-pass tests first**

Create `crates/bus-macros/tests/derive_test.rs`:

```rust
use bus_core::{Event, MessageId};
use bus_macros::Event;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// Basic derive with static subject
#[derive(Debug, Serialize, Deserialize, Event)]
#[event(subject = "orders.created")]
struct OrderCreated {
    id: MessageId,
    total: i64,
}

#[test]
fn static_subject() {
    let evt = OrderCreated { id: MessageId::new(), total: 100 };
    assert_eq!(evt.subject(), "orders.created");
}

#[test]
fn message_id_returns_id_field() {
    let id = MessageId::new();
    let evt = OrderCreated { id: id.clone(), total: 50 };
    assert_eq!(evt.message_id(), id);
}

#[test]
fn default_aggregate_type() {
    assert_eq!(OrderCreated::aggregate_type(), "default");
}

// Derive with field interpolation in subject
#[derive(Debug, Serialize, Deserialize, Event)]
#[event(subject = "orders.{self.order_id}.shipped", aggregate = "order")]
struct OrderShipped {
    id: MessageId,
    order_id: Uuid,
}

#[test]
fn interpolated_subject() {
    let order_id = Uuid::now_v7();
    let evt = OrderShipped { id: MessageId::new(), order_id };
    assert_eq!(evt.subject(), format!("orders.{}.shipped", order_id));
}

#[test]
fn custom_aggregate_type() {
    assert_eq!(OrderShipped::aggregate_type(), "order");
}
```

- [ ] **Step 2: Run tests to confirm they fail (stub generates empty impl)**

```bash
cargo test -p bus-macros --test derive_test 2>&1 | head -30
```

Expected: compile error — `Event` trait not implemented on `OrderCreated`.

- [ ] **Step 3: Implement `crates/bus-macros/src/event.rs`**

```rust
use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{
    parse_macro_input, Data, DeriveInput, Expr, ExprLit, Fields, Lit, Meta,
};

pub fn derive_event_impl(input: TokenStream) -> TokenStream {
    let ast = parse_macro_input!(input as DeriveInput);
    match generate(&ast) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

fn generate(ast: &DeriveInput) -> syn::Result<TokenStream2> {
    let name = &ast.ident;

    // Parse #[event(...)] attribute
    let event_attr = ast
        .attrs
        .iter()
        .find(|a| a.path().is_ident("event"))
        .ok_or_else(|| {
            syn::Error::new_spanned(
                name,
                "missing #[event(subject = \"...\")] attribute",
            )
        })?;

    let mut subject_template: Option<String> = None;
    let mut aggregate: String = "default".into();

    event_attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("subject") {
            let value = meta.value()?;
            let lit: Lit = value.parse()?;
            if let Lit::Str(s) = lit {
                subject_template = Some(s.value());
            }
            Ok(())
        } else if meta.path.is_ident("aggregate") {
            let value = meta.value()?;
            let lit: Lit = value.parse()?;
            if let Lit::Str(s) = lit {
                aggregate = s.value();
            }
            Ok(())
        } else {
            Err(meta.error("unknown event attribute key"))
        }
    })?;

    let subject_template = subject_template.ok_or_else(|| {
        syn::Error::new_spanned(name, "subject = \"...\" is required in #[event(...)]")
    })?;

    // Validate struct has `id: MessageId` field
    let fields = match &ast.data {
        Data::Struct(s) => match &s.fields {
            Fields::Named(f) => &f.named,
            _ => {
                return Err(syn::Error::new_spanned(
                    name,
                    "#[derive(Event)] requires a struct with named fields",
                ))
            }
        },
        _ => {
            return Err(syn::Error::new_spanned(
                name,
                "#[derive(Event)] can only be applied to structs",
            ))
        }
    };

    let has_id = fields.iter().any(|f| {
        f.ident.as_ref().map(|i| i == "id").unwrap_or(false)
    });
    if !has_id {
        return Err(syn::Error::new_spanned(
            name,
            "#[derive(Event)] requires a field named `id: bus_core::MessageId`",
        ));
    }

    // Generate subject() body from template
    // Template syntax: "orders.{self.field}.created" → format!("orders.{}.created", self.field)
    let subject_body = build_subject_expr(&subject_template, name)?;

    let expanded = quote! {
        impl bus_core::Event for #name {
            fn subject(&self) -> ::std::borrow::Cow<'_, str> {
                ::std::borrow::Cow::Owned(#subject_body)
            }

            fn message_id(&self) -> bus_core::MessageId {
                self.id.clone()
            }

            fn aggregate_type() -> &'static str {
                #aggregate
            }
        }
    };

    Ok(expanded)
}

/// Parse template like "orders.{self.field}.created" into a `format!` expression.
fn build_subject_expr(template: &str, span_target: &syn::Ident) -> syn::Result<TokenStream2> {
    let mut format_str = String::new();
    let mut args: Vec<TokenStream2> = Vec::new();
    let mut rest = template;

    while let Some(open) = rest.find('{') {
        format_str.push_str(&rest[..open]);
        rest = &rest[open + 1..];

        let close = rest.find('}').ok_or_else(|| {
            syn::Error::new_spanned(span_target, "unclosed `{` in subject template")
        })?;

        let expr_str = rest[..close].trim();
        rest = &rest[close + 1..];

        // Only support {self.field} form
        if !expr_str.starts_with("self.") {
            return Err(syn::Error::new_spanned(
                span_target,
                format!(
                    "subject interpolations must use `{{self.field}}` form, got `{{{}}}`",
                    expr_str
                ),
            ));
        }

        let field_name = &expr_str["self.".len()..];
        let field_ident: syn::Ident = syn::parse_str(field_name).map_err(|_| {
            syn::Error::new_spanned(
                span_target,
                format!("invalid field name in subject template: `{}`", field_name),
            )
        })?;

        format_str.push_str("{}");
        args.push(quote! { self.#field_ident });
    }

    format_str.push_str(rest);

    if args.is_empty() {
        Ok(quote! { #format_str.to_owned() })
    } else {
        Ok(quote! { format!(#format_str, #(#args),*) })
    }
}
```

- [ ] **Step 4: Run tests to confirm they pass**

```bash
cargo test -p bus-macros --test derive_test
```

Expected:
```
test static_subject ... ok
test message_id_returns_id_field ... ok
test default_aggregate_type ... ok
test interpolated_subject ... ok
test custom_aggregate_type ... ok
```

- [ ] **Step 5: Commit**

```bash
git add crates/bus-macros/src/event.rs crates/bus-macros/tests/derive_test.rs
git commit -m "feat(bus-macros): implement #[derive(Event)] with subject template and field interpolation"
```

---

## Task 11: Add compile-fail tests for `#[derive(Event)]`

**Files:**
- Create: `crates/bus-macros/tests/compile_fail/missing_id.rs`
- Create: `crates/bus-macros/tests/compile_fail/missing_id.stderr`
- Create: `crates/bus-macros/tests/compile_fail/missing_subject.rs`
- Create: `crates/bus-macros/tests/compile_fail/missing_subject.stderr`
- Modify: `crates/bus-macros/Cargo.toml` — add `trybuild` dev-dependency
- Create: `crates/bus-macros/tests/compile_fail_tests.rs`

- [ ] **Step 1: Add `trybuild` to dev-dependencies**

Edit `crates/bus-macros/Cargo.toml`, append under `[dev-dependencies]`:

```toml
trybuild = { version = "1", features = ["diff"] }
```

- [ ] **Step 2: Create compile-fail fixture — missing `id` field**

Create `crates/bus-macros/tests/compile_fail/missing_id.rs`:

```rust
use bus_macros::Event;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Event)]
#[event(subject = "orders.created")]
struct BadEvent {
    total: i64,  // missing id: MessageId
}

fn main() {}
```

- [ ] **Step 3: Create compile-fail fixture — missing `#[event]` attribute**

Create `crates/bus-macros/tests/compile_fail/missing_subject.rs`:

```rust
use bus_macros::Event;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Event)]
struct BadEvent {
    id: bus_core::MessageId,
}

fn main() {}
```

- [ ] **Step 4: Create test runner**

Create `crates/bus-macros/tests/compile_fail_tests.rs`:

```rust
#[test]
fn compile_fail_cases() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile_fail/missing_id.rs");
    t.compile_fail("tests/compile_fail/missing_subject.rs");
}
```

- [ ] **Step 5: Run once to generate `.stderr` snapshots**

```bash
cargo test -p bus-macros --test compile_fail_tests 2>&1
```

Expected: first run may fail with "stderr mismatch" — this generates the `.stderr` snapshot files in `crates/bus-macros/tests/compile_fail/`. Rerun:

```bash
cargo test -p bus-macros --test compile_fail_tests
```

Expected: `test compile_fail_cases ... ok`

- [ ] **Step 6: Commit**

```bash
git add crates/bus-macros/
git commit -m "test(bus-macros): add trybuild compile-fail tests for missing id and subject"
```

---

## Task 12: Full workspace check and final integration test

**Files:**
- No new files

- [ ] **Step 1: Run all tests across workspace**

```bash
cargo test --workspace
```

Expected: all tests pass, zero warnings about unused imports.

- [ ] **Step 2: Run clippy**

```bash
cargo clippy --workspace -- -D warnings
```

Expected: no warnings. If any, fix before continuing.

- [ ] **Step 3: Verify `bus-core` has no NATS or sqlx dependencies**

```bash
cargo tree -p bus-core --edges normal
```

Expected: dependency tree contains only `async-trait`, `serde`, `serde_json`, `thiserror`, `tracing`, `uuid` and their transitive deps. No `async-nats`, `sqlx`, `rusqlite`.

- [ ] **Step 4: Final commit**

```bash
git add -A
git commit -m "chore: plan-1 complete — bus-core traits + bus-macros derive macro verified"
```

---

## Summary

After Plan 1 is complete you will have:

- `bus-core`: `Event`, `Publisher`, `EventHandler`, `IdempotencyStore` traits; `MessageId`, `PubReceipt`, `HandlerCtx`, `BusError`, `HandlerError` types — zero NATS/sqlx deps
- `bus-macros`: `#[derive(Event)]` with subject template interpolation, compile-time validation, and trybuild compile-fail coverage
- All `cargo test --workspace` passing, `cargo clippy` clean

**Plan 2** covers `bus-nats` (JetStream publisher + subscriber + inbox + DLQ + circuit breaker) and `bus-outbox` (Postgres outbox + SQLite buffer + dispatcher) — requires Docker for testcontainers.

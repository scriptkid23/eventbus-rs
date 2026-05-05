# JetStream advisory logging — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement an opt-in `bus-nats::advisory` task that subscribes to `$JS.EVENT.ADVISORY.>` (configurable), logs each message with `tracing` (JSON field extraction when possible), and reconnects with exponential backoff on stream failure.

**Architecture:** A thin module in `crates/bus-nats/src/advisory.rs`: (1) pure `extract_advisory_fields` for testability; (2) `AdvisoryLogOptions` + `AdvisoryLoggerHandle` (abort-on-drop); (3) `spawn_jetstream_advisory_logger` validates the first `Client::subscribe`, spawns a Tokio task that runs `subscriber.next()` until the stream ends, then sleeps with backoff and resubscribes forever until aborted.

**Tech Stack:** Rust 2024 workspace, `async-nats` 0.46 (`jetstream::Context::client()`, `Client::subscribe`), `tokio`, `tracing`, `serde_json`, `futures-util::StreamExt`, `bus_core::BusError`.

---

## File map

| File | Role |
|------|------|
| `crates/bus-nats/src/advisory.rs` | Types, `extract_advisory_fields`, spawn + loop, unit tests |
| `crates/bus-nats/src/lib.rs` | `pub use advisory::{…}` |

**Spec:** [`docs/superpowers/specs/2026-05-05-jetstream-advisory-logging-design.md`](../specs/2026-05-05-jetstream-advisory-logging-design.md)

---

### Task 1: Advisory payload extraction (pure logic + unit tests)

**Files:**

- Modify: `crates/bus-nats/src/advisory.rs` (replace stub)

- [ ] **Step 1: Replace `advisory.rs` with extraction helper + inline unit tests**

Replace the contents of `crates/bus-nats/src/advisory.rs` with the following (Task 2 merges spawn code into the same file).

```rust
//! JetStream advisory logging (see crate re-exports).

/// Default NATS subject wildcard for JetStream advisories (excludes metrics).
pub const DEFAULT_ADVISORY_SUBJECT_FILTER: &str = "$JS.EVENT.ADVISORY.>";

/// Fields extracted from an advisory JSON payload for structured logging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExtractedAdvisoryFields {
    pub(crate) advisory_type: Option<String>,
    pub(crate) stream: Option<String>,
    pub(crate) consumer: Option<String>,
    pub(crate) json_ok: bool,
    pub(crate) payload_len: usize,
}

/// Best-effort parse: object with string keys `type`, `stream`, `consumer`.
pub(crate) fn extract_advisory_fields(payload: &[u8]) -> ExtractedAdvisoryFields {
    let payload_len = payload.len();
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(payload) else {
        return ExtractedAdvisoryFields {
            advisory_type: None,
            stream: None,
            consumer: None,
            json_ok: false,
            payload_len,
        };
    };
    let Some(obj) = value.as_object() else {
        return ExtractedAdvisoryFields {
            advisory_type: None,
            stream: None,
            consumer: None,
            json_ok: true,
            payload_len,
        };
    };
    let str_or_none = |k: &str| {
        obj.get(k)
            .and_then(|v| v.as_str())
            .map(std::string::ToString::to_string)
    };
    ExtractedAdvisoryFields {
        advisory_type: str_or_none("type"),
        stream: str_or_none("stream"),
        consumer: str_or_none("consumer"),
        json_ok: true,
        payload_len,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_invalid_json() {
        let b = [0xff, 0xfe, 0xfd];
        let e = extract_advisory_fields(&b);
        assert!(!e.json_ok);
        assert_eq!(e.payload_len, 3);
        assert!(e.advisory_type.is_none());
    }

    #[test]
    fn extract_full_object() {
        let j = br#"{"type":"io.nats.jetstream.advisory.v1.max_deliver","stream":"EVENTS","consumer":"w1"}"#;
        let e = extract_advisory_fields(j);
        assert!(e.json_ok);
        assert_eq!(
            e.advisory_type.as_deref(),
            Some("io.nats.jetstream.advisory.v1.max_deliver")
        );
        assert_eq!(e.stream.as_deref(), Some("EVENTS"));
        assert_eq!(e.consumer.as_deref(), Some("w1"));
    }

    #[test]
    fn extract_array_json_no_keys() {
        let j = br#"[1,2]"#;
        let e = extract_advisory_fields(j);
        assert!(e.json_ok);
        assert!(e.advisory_type.is_none());
    }

    #[test]
    fn extract_non_string_type_ignored() {
        let j = br#"{"type":1}"#;
        let e = extract_advisory_fields(j);
        assert!(e.json_ok);
        assert!(e.advisory_type.is_none());
    }
}
```

- [ ] **Step 2: Run unit tests**

Run:

```bash
cargo test -p bus-nats extract_
```

Expected: all four tests **PASS**.

- [ ] **Step 3: Commit**

```bash
git add crates/bus-nats/src/advisory.rs
git commit -m "test(bus-nats): add JetStream advisory payload field extraction"
```

---

### Task 2: Options, handle, spawn + logging loop

**Files:**

- Modify: `crates/bus-nats/src/advisory.rs`

- [ ] **Step 1: Prepend module documentation, imports, and tracing target**

Replace the existing one-line `//!` at the top of `advisory.rs` with the following block, then add the `use` lines immediately after it:

```rust
//! JetStream advisory logging: opt-in background task that subscribes to
//! `$JS.EVENT.ADVISORY.>` (configurable) and emits structured [`tracing`] events
//! (target `bus_nats::jetstream_advisory`). Requires NATS permission to subscribe
//! to the chosen pattern.
```

```rust
use std::time::Duration;

use async_nats::jetstream;
use bus_core::error::BusError;
use futures_util::StreamExt;
use tokio::task::JoinHandle;

const TRACING_TARGET: &str = "bus_nats::jetstream_advisory";
```

(If you add `.instrument(...)` on the spawned task, also `use tracing::Instrument;`. If clippy complains or the crate does not use spans elsewhere, omit `Instrument` and `.instrument(...)`.)

- [ ] **Step 2: Add `AdvisoryLogOptions` and `AdvisoryLoggerHandle`**

Insert after `DEFAULT_ADVISORY_SUBJECT_FILTER` (before `ExtractedAdvisoryFields` is fine, or reorder so public types come first):

```rust
/// Options for [`spawn_jetstream_advisory_logger`].
#[derive(Debug, Clone)]
pub struct AdvisoryLogOptions {
    /// Core NATS wildcard, e.g. [`DEFAULT_ADVISORY_SUBJECT_FILTER`].
    pub subject_filter: String,
    /// Max bytes of lossy UTF-8 prefix logged at `debug` when JSON parse fails.
    pub max_payload_bytes_log: usize,
}

impl Default for AdvisoryLogOptions {
    fn default() -> Self {
        Self {
            subject_filter: DEFAULT_ADVISORY_SUBJECT_FILTER.to_string(),
            max_payload_bytes_log: 256,
        }
    }
}

/// Drop aborts the background advisory logging task.
pub struct AdvisoryLoggerHandle {
    _handle: JoinHandle<()>,
}

impl Drop for AdvisoryLoggerHandle {
    fn drop(&mut self) {
        self._handle.abort();
    }
}
```

- [ ] **Step 3: Add `log_one_advisory` (uses `extract_advisory_fields`)**

```rust
fn log_one_advisory(subject: &str, payload: &[u8], max_payload_bytes_log: usize) {
    let ext = extract_advisory_fields(payload);
    if !ext.json_ok {
        tracing::warn!(
            target: TRACING_TARGET,
            subject = subject,
            payload_len = ext.payload_len,
            "jetstream advisory payload is not JSON"
        );
        if max_payload_bytes_log > 0 {
            let prefix_len = max_payload_bytes_log.min(payload.len());
            let preview = String::from_utf8_lossy(&payload[..prefix_len]);
            tracing::debug!(
                target: TRACING_TARGET,
                subject = subject,
                preview = %preview,
                "jetstream advisory raw prefix (lossy UTF-8)"
            );
        }
        return;
    }

    tracing::info!(
        target: TRACING_TARGET,
        subject = subject,
        advisory_type = ext.advisory_type.as_deref(),
        stream = ext.stream.as_deref(),
        consumer = ext.consumer.as_deref(),
        payload_len = ext.payload_len,
        "jetstream advisory"
    );
}
```

- [ ] **Step 4: Add `spawn_jetstream_advisory_logger`**

```rust
/// Subscribe to JetStream advisory subjects and log each message via [`tracing`].
///
/// Call once per NATS connection (or process). Requires the NATS user to have
/// subscribe permission on the configured pattern (default `$JS.EVENT.ADVISORY.>`).
///
/// On subscription stream failure, reconnects with exponential backoff (200ms .. 30s).
pub async fn spawn_jetstream_advisory_logger(
    js: &jetstream::Context,
    opts: AdvisoryLogOptions,
) -> Result<AdvisoryLoggerHandle, BusError> {
    let client = js.client();
    let filter = opts.subject_filter.clone();
    let max_payload_bytes_log = opts.max_payload_bytes_log;

    let mut subscriber = client
        .subscribe(filter.clone())
        .await
        .map_err(|e| BusError::Nats(e.to_string()))?;

    let client = client.clone();
    let handle = tokio::spawn(async move {
        let mut backoff = Duration::from_millis(200);
        loop {
            while let Some(message) = subscriber.next().await {
                let subject = message.subject.as_str();
                log_one_advisory(subject, &message.payload, max_payload_bytes_log);
            }

            tracing::error!(
                target: TRACING_TARGET,
                "jetstream advisory subscription stream ended; reconnecting after backoff"
            );
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(30));

            loop {
                match client.subscribe(filter.clone()).await {
                    Ok(sub) => {
                        subscriber = sub;
                        backoff = Duration::from_millis(200);
                        break;
                    }
                    Err(e) => {
                        tracing::error!(
                            target: TRACING_TARGET,
                            error = %e,
                            "jetstream advisory resubscribe failed; retrying after backoff"
                        );
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(Duration::from_secs(30));
                    }
                }
            }
        }
    }
    .instrument(tracing::debug_span!(
        target: TRACING_TARGET,
        "jetstream_advisory_logger"
    )));

    Ok(AdvisoryLoggerHandle { _handle: handle })
}
```

- [ ] **Step 6: Remove duplicate `//!`**

Ensure only one module doc block exists at the top (delete the old one-line `//! JetStream advisory logging (see crate re-exports).` if it remains below the imports).

- [ ] **Step 7: Rearranging public types (optional)**

If you prefer public types before `pub(crate)` helpers, move `AdvisoryLogOptions`, `AdvisoryLoggerHandle`, `DEFAULT_ADVISORY_SUBJECT_FILTER`, and `spawn_jetstream_advisory_logger` above `ExtractedAdvisoryFields` — purely organizational.

- [ ] **Step 8: `cargo fmt` + `cargo clippy` + tests**

```bash
cargo fmt --all
cargo clippy -p bus-nats --all-features -- -D warnings
cargo test -p bus-nats
```

Expected: clean clippy, tests pass.

- [ ] **Step 9: Commit**

```bash
git add crates/bus-nats/src/advisory.rs
git commit -m "feat(bus-nats): spawn JetStream advisory logger with tracing"
```

---

### Task 3: Public re-exports

**Files:**

- Modify: `crates/bus-nats/src/lib.rs`

- [ ] **Step 1: Re-export advisory types**

After `pub mod advisory;`, add:

```rust
pub use advisory::{
    AdvisoryLoggerHandle, AdvisoryLogOptions, DEFAULT_ADVISORY_SUBJECT_FILTER,
    spawn_jetstream_advisory_logger,
};
```

- [ ] **Step 2: Verify workspace**

```bash
cargo build --workspace --all-features
cargo test --workspace
```

Expected: success.

- [ ] **Step 3: Commit**

```bash
git add crates/bus-nats/src/lib.rs
git commit -m "feat(bus-nats): re-export JetStream advisory logger API"
```

---

### Task 4 (optional): README note

**Files:**

- Modify: `README.md` (§8 Observability or a one-line bullet)

- [ ] **Step 1:** Add one sentence that `bus_nats::spawn_jetstream_advisory_logger` exists for JetStream advisories, link to NATS docs for subject layout — only if maintainers want user-visible discovery.

- [ ] **Step 2: Commit** (skip entire task if not needed)

```bash
git commit -m "docs: mention JetStream advisory logging helper"
```

---

## Plan self-review (spec coverage)

| Spec § | Covered by |
|--------|------------|
| 5.1 `AdvisoryLogOptions` + default filter | Task 2 |
| 5.1 `max_payload_bytes_log` | Task 2 |
| 5.1 `AdvisoryLoggerHandle` drop abort | Task 2 |
| 5.2 async spawn + `BusError::Nats` on first subscribe | Task 2 |
| 6.1 target + `info!` | Task 2 `TRACING_TARGET`, `log_one_advisory` |
| 6.2 fields + warn on non-JSON | Task 2 |
| 6.3 error log + backoff resubscribe | Task 2 loop |
| 7 permissions rustdoc | Task 2 module doc |
| 8 unit tests | Task 1 (+ Task 2 integration implicitly via build) |
| 10 `lib.rs` re-export | Task 3 |

**Note:** First successful subscribe happens inside `spawn` before `tokio::spawn`; resubscribe failures only log + backoff (per spec §6.3).

---

## Execution handoff

**Plan complete and saved to `docs/superpowers/plans/2026-05-05-jetstream-advisory-logging.md`. Two execution options:**

1. **Subagent-Driven (recommended)** — dispatch a fresh subagent per task, review between tasks, fast iteration  
2. **Inline execution** — run tasks in this session with executing-plans, batch with checkpoints  

**Which approach do you want?**

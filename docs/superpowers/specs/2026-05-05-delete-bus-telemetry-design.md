# Delete `bus-telemetry` crate — Design

**Status:** Proposed
**Date:** 2026-05-05
**Scope:** workspace-wide (delete one crate; clean dependents, README, diagrams)
**Follows on from:** [`2026-05-05-slim-eventbus-rs-to-transport-design.md`](2026-05-05-slim-eventbus-rs-to-transport-design.md)

## 1. Context and goal

`bus-telemetry` was added as the home for OpenTelemetry instrumentation
(metrics, spans, W3C `traceparent` propagation through NATS headers). After the
recent slim-down of the workspace it is the only "Planned, not Shipped"
component left. Auditing the current state shows three problems:

1. **Zero integration.** No code in `bus-nats`, `event-bus`, or `bus-core`
   imports `bus_telemetry`. `bus-nats/Cargo.toml` does not depend on it. The
   `event-bus` `otel` feature pulls the crate into the dependency graph but
   nothing calls into it, so enabling `--features otel` produces no spans, no
   metrics, and no header propagation.
2. **Code rot from the slim-down.** `spans.rs` still defines
   `outbox_dispatch_span()` and a marker-only `pub struct SpanBuilder;`
   reserved "for a future builder API". `idempotency_span()` has no callers.
   Only `propagation::{inject,extract}_context` and `metrics::BusMetrics` are
   useful primitives, and even those are unused.
3. **Overpromised README.** The implementation-status table lists "OTel spans
   + metrics — Planned"; §8 Observability ships an entire metrics table
   (`eventbus.publish.total`, `eventbus.handle.duration`,
   `eventbus.dlq.total`, `eventbus.jetstream.advisory.total`) with no
   producer behind it, plus a multi-bullet "Planned advisory observability"
   section that references the same `otel` feature path. The Features list
   bullet on line 63 also says "Observable. … (planned in `bus-telemetry`)".

Rather than wire OTel up properly (a project-sized effort touching every
publish/consume code path), the decision is to **delete `bus-telemetry`**
together with every README/diagram claim that relies on it. Users who need
OpenTelemetry can wire `tracing-opentelemetry` against the existing
`tracing::*!` events in `bus-nats` themselves; the crate's `bus-core`
traits do not need to change for that.

This matches the slim-down ethos already applied to `bus-outbox`: ship only
what works; remove "Planned" stubs that have no code behind them.

### Goals

- Remove `bus-telemetry` from the workspace entirely.
- Strip the `otel` feature and the optional `bus-telemetry` dependency from
  `event-bus`.
- Update the README so no surviving sentence references OTel, the `otel`
  feature, `bus-telemetry`, or any metric that was only going to exist via
  that crate.
- Update `docs/diagrams/` so the architecture pictures match the new reality.

### Non-goals

- Replacing `bus-telemetry` with another instrumentation crate.
- Adding inline guidance in the README for "how to wire OTel yourself"
  (consistent with §6.2 of the prior slim spec — no code samples, no
  recommended libraries).
- Removing the `tracing` crate or any `tracing::*!` calls from `bus-nats`
  — those are structured logging events, not OTel, and they continue to
  work without `bus-telemetry`.
- Cleaning up the OpenTelemetry workspace dependencies in the root
  `Cargo.toml` (`opentelemetry`, `opentelemetry_sdk`,
  `opentelemetry-semantic-conventions`, `tracing-opentelemetry`). These
  become unused after this change but are harmless; cleanup follows the same
  precedent as §617 of the prior slim spec, which left `sqlx`, `rusqlite`,
  and `testcontainers-modules.postgres` in place.

## 2. Crate layout after change

```
crates/
├── bus-core         (UNCHANGED)
├── bus-macros       (UNCHANGED)
├── bus-nats         (UNCHANGED)
└── event-bus        (drop `otel` feature + bus-telemetry dep)

REMOVED:
└── bus-telemetry    (entire crate deleted)
```

No code under `crates/event-bus/src/` references `bus_telemetry`, so the only
`event-bus` change is in `Cargo.toml`. No `bus-nats` change is needed.

## 3. `event-bus` Cargo manifest changes

In `crates/event-bus/Cargo.toml`:

- **Remove** the `otel = ["dep:bus-telemetry"]` line from `[features]`.
- **Remove** the `bus-telemetry = { path = "../bus-telemetry", optional = true }`
  line from `[dependencies]`.

Resulting `[features]` block:

```toml
[features]
default       = ["macros", "nats-kv-inbox"]
macros        = ["dep:bus-macros"]
nats-kv-inbox = ["bus-nats/nats-kv-inbox"]
redis-inbox   = ["bus-nats/redis-inbox"]
sqlite-buffer = ["bus-nats/sqlite-buffer"]
```

No other change in the manifest. `dev-dependencies` stays as-is.

## 4. Workspace manifest changes

In root `Cargo.toml`, remove `"crates/bus-telemetry"` from
`workspace.members`. Resulting members list:

```toml
[workspace]
members = [
    "crates/bus-core",
    "crates/bus-macros",
    "crates/bus-nats",
    "crates/event-bus",
    "examples/01-basic-publish",
    "examples/03-idempotent-handler",
]
resolver = "2"
```

`[workspace.dependencies]` is **not** modified — the OTel deps stay there
(see Non-goals).

## 5. Files removed

- `crates/bus-telemetry/` — entire directory (`src/`, `tests/`, `Cargo.toml`).

That is the only directory removed. There are no examples or migrations
specific to `bus-telemetry`.

## 6. README rewrite

The README currently makes five distinct claims tied to `bus-telemetry`.
All five are removed in this change.

### 6.1 Sections to change

| Section | Change |
|---|---|
| Features list (line 63) | **Remove** the `**Observable.**` bullet. The bullet's only content is the OTel claim; deleting it leaves the rest of the feature list intact. |
| Cargo features table (line 350) | Remove the `\| `event-bus` \| `otel` \| no \| OpenTelemetry spans + metrics (via `bus-telemetry`) \|` row. |
| §8 Observability (lines 318-338) | **Replace** the entire body with a short honest paragraph (see §6.2 below). The current text references the `otel` feature, claims metrics that have no producer, and lists "Planned advisory observability" bullets that all chain back to `bus-telemetry`. None of that survives. |
| Implementation status table (line 409) | Remove the row `\| OTel spans + metrics \| `bus-telemetry` \| 📋 Planned \|`. |
| Roadmap v0.2 (line 451) | Change `**v0.2** — `crates.io` publish, OTel spans + metrics.` to `**v0.2** — `crates.io` publish.`. |

### 6.2 New §8 Observability — verbatim wording target

The replacement section is intentionally short and link-free. Suggested
content:

> ### 8. Observability
>
> `bus-nats` emits structured `tracing` events at `info` / `warn` / `error`
> for publish, consume, retry, idempotency-store outcomes, and DLQ
> handoff. Wire your preferred `tracing-subscriber` layer (JSON, OTLP, …)
> in your application bootstrap to forward those to whatever observability
> stack you run. `eventbus-rs` itself does not bundle an OpenTelemetry
> exporter or define its own metrics.

No metrics table, no advisory roadmap bullets, no `traceparent` header
guarantee. The point is to set correct expectations and stop promising
features that do not exist in the workspace.

### 6.3 Other audit points

After applying the section edits, `grep` the README for these strings; each
remaining hit must either be removed or be inside a code block / quoted
identifier where the text is unrelated to the deleted crate:

- `bus-telemetry`
- `bus_telemetry`
- ` otel ` (with surrounding spaces, to avoid matching unrelated tokens)
- `OpenTelemetry`
- `traceparent`
- `eventbus.publish.total`
- `eventbus.consume.total`
- `eventbus.handle.duration`
- `eventbus.dlq.total`
- `eventbus.jetstream.advisory.total`
- `OTel`

Hits in [`docs/superpowers/specs/`](.) (this spec or any prior one) are
expected — leave those alone.

## 7. Architecture diagram updates

### 7.1 `docs/diagrams/component-diagram.md`

Mermaid block currently includes:

- A subgraph block `subgraph telemetry [bus-telemetry optional]` containing
  `inject_context`, `extract_context`, `publish and consume metrics` nodes
  (lines 39-43).
- An `otel[("OTel collector")]` node inside the `external` subgraph
  (line 49).
- Four edges wiring publisher/subscriber to the telemetry subgraph and out
  to the OTel collector:
  - `natsPublisher --> inject` (line 75)
  - `subscriber --> extract` (line 76)
  - `extract --> metrics` (line 77)
  - `metrics --> otel` (line 78)

**Action:** Remove all six items above from the mermaid block. Other
nodes, subgraphs, and edges are untouched. The "Key Flows" prose section
underneath has no telemetry references and is left as-is.

### 7.2 `docs/diagrams/system-diagrams.md`

Three of the four sections reference `bus-telemetry` or the OTel collector:

- **§1 Workspace Dependency Graph** (lines 5-22): remove the `busTelemetry`
  node and the two edges `busTelemetry --> busCore` and
  `eventBus -->|"optional feature: otel"| busTelemetry`.
- **§2 event-bus Feature Map** (lines 24-40): remove the
  `otel["otel -> bus-telemetry"]` node and the `eventBus --> otel` edge.
- **§4 Telemetry Propagation** (lines 73-87): remove the entire `## 4`
  section, including its mermaid block. Renumbering is not required —
  there is no §5.

§3 Publish and Consume Flow has no telemetry references and is unchanged.

## 8. Versioning

Stay at `0.1.0`. Same reasoning as the prior slim spec: never published to
`crates.io`, no downstream consumers, the next published version is the
meaningful marker.

## 9. Sequencing

Single coherent PR, ordered so the workspace builds at every checkpoint.
Per-task commits prefixed `slim:` to group with the prior slim work.

1. **Update `event-bus/Cargo.toml`.** Remove the `otel` feature line and the
   `bus-telemetry` dep line. Verify `cargo build -p event-bus --all-features`
   succeeds. (The `bus-telemetry` crate still exists on disk at this point
   but is no longer reachable from `event-bus`.)
2. **Update root `Cargo.toml`.** Remove `"crates/bus-telemetry"` from
   `workspace.members`.
3. **Delete the crate.** `git rm -r crates/bus-telemetry`. Verify
   `cargo build --workspace --all-features` succeeds.
4. **Rewrite README per §6.** Five distinct edits (Features bullet,
   features table row, §8 body, status row, roadmap line). Run the §6.3
   grep audit and fix any leftover.
5. **Update `docs/diagrams/component-diagram.md`** per §7.1.
6. **Update `docs/diagrams/system-diagrams.md`** per §7.2.
7. **Verify clean.** `cargo fmt --all`, `cargo clippy --workspace
   --all-features -- -D warnings`, `cargo build --workspace --all-features`,
   `cargo test --workspace`. The deleted `propagation_test.rs` (3 tests)
   is no longer collected; the rest of the workspace test count is
   unchanged.

## 10. What this change does NOT do

To prevent scope creep during implementation:

- Does not modify `bus-core`, `bus-macros`, or `bus-nats` source code.
- Does not modify `event-bus` source code (`crates/event-bus/src/**`).
  Only the manifest changes.
- Does not remove the OTel workspace dependencies from root `Cargo.toml`.
- Does not add a "how to wire your own OTel" guide to the README.
- Does not bump the workspace version.
- Does not publish to `crates.io`.
- Does not touch `tracing::*!` calls in `bus-nats`.

## 11. Risks and mitigations

| Risk | Mitigation |
|---|---|
| A consumer of the `event-bus` crate has `features = ["otel"]` in their `Cargo.toml` and the build breaks | The crate has never been published to `crates.io`. The only known consumers are this workspace's two examples, neither of which uses `otel`. Acceptable. |
| Implementation-status row removal makes the table look incomplete | Verify after edit: the only "Planned" row left is `crates.io` publish, which is genuinely planned. |
| README §8 grep audit (§6.3) misses a stale reference | The grep list is explicit. Any hit outside a spec file is a bug to fix during step 4. |
| Workspace OTel deps left in `Cargo.toml.workspace.dependencies` confuse a future reader | Acceptable, same precedent as `sqlx`/`rusqlite` after the slim-down. A separate cleanup PR can tackle workspace-wide unused deps later. |
| Loss of "Observable / OTel planned" framing makes the project look less production-ready in marketing terms | Trade accepted: shipping less and being honest beats over-promising. The Roadmap can re-add OTel (as a Shipped item, not a Planned bullet) when somebody actually wires it up. |

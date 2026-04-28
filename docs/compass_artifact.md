# Hướng dẫn toàn diện: Đảm bảo exactly-once delivery với NATS JetStream trong Rust

**Bottom line up front:** NATS JetStream **không** cung cấp exactly-once delivery thật sự — về mặt lý thuyết điều này là bất khả thi (Two Generals Problem). Cái mà JetStream cung cấp là **"effectively-once semantics"** = at-least-once delivery + server-side deduplication trong một cửa sổ thời gian giới hạn (mặc định 2 phút) + double-ack từ consumer. Để đạt được "không mất, không trùng" ở mức production cho business-critical events, bạn **bắt buộc** phải lớp thêm các pattern ứng dụng: **Transactional Outbox** ở producer, **Inbox/Idempotency store** ở consumer, **DLQ** cho poison messages, và **fallback queue cục bộ** khi NATS không khả dụng. Bài viết này trình bày chi tiết từng lớp với code Rust sẵn sàng production, dựa trên phát hiện từ Synadia, báo cáo Jepsen 2025 (đã chỉ ra mất ~14% messages khi mất điện đồng thời nhiều node ở mức cấu hình mặc định), và case studies từ Form3, Tinder, NVIDIA Cloud Functions.

---

## 1. JetStream exactly-once: cơ chế thật sự đằng sau marketing

### 1.1 Cách dedup phía publisher hoạt động

Khi client gửi message với header **`Nats-Msg-Id: <id-duy-nhất>`**, server JetStream lưu ID này trong một **dedup table in-memory** (Go map, O(1) lookup) gắn với từng stream. Lần publish thứ hai cùng Msg-Id trả về `PubAck { duplicate: true, sequence: <seq-gốc> }` mà **không append** message mới. Đây là cơ chế đơn giản nhưng quan trọng.

Cấu hình ở mức stream — field **`duplicate_window`** (mặc định **120 giây = 2 phút**, có thể chỉnh qua `--dupe-window` hoặc API). Bộ nhớ tốn ≈ `(rate × window) × (id_size + overhead)`; Synadia cảnh báo rõ trong NATS Weekly #34: "với rate cao, memory tăng nhanh nên window phải nhỏ; chúng tôi khuyến cáo không dùng window lớn." Dedup state chỉ ở **RAM**, không persist trên disk; restart hoặc leader failover có thể restore từ Raft snapshot nhưng đây là *best-effort* — đừng dựa vào dedup window cross-restart cho durability dài hạn.

Cho dedup **vô hạn** (BYOID across all time), pattern đề xuất bởi Synadia là nhúng ID vào **subject name** kết hợp `DiscardNewPerSubject` + `MaxMsgsPerSubject=1` (NATS server ≥ 2.9). Stream sẽ enforce "chỉ một message duy nhất cho mỗi subject" mãi mãi — đánh đổi memory dedup table bằng cardinality của subject space.

### 1.2 Các loại Ack và ngữ nghĩa

| AckPolicy | Hành vi | Use case |
|---|---|---|
| **`AckExplicit`** | Mỗi message phải được ack/nak rõ ràng; bắt buộc cho pull consumers | Mặc định cho durable workloads |
| **`AckAll`** | Ack seq N ngầm ack tất cả seq ≤ N | Chỉ an toàn khi xử lý strict in-order |
| **`AckNone`** | Server coi delivery = ack | Telemetry, fire-and-forget |

Các loại reply ack qua wire: `+ACK` (success), `-NAK` với optional delay, `+WPI` (in-progress, gia hạn AckWait), `+TERM` (kết thúc, không redeliver — emit `MSG_TERMINATED` advisory), `+NXT` (ack-and-pull-next, chỉ pull).

**Quan trọng:** `AckAll` *không* nghĩa là "tôi đã xử lý tất cả message trước đó"; nó chỉ tắt redelivery. Nếu handler skip một message do lỗi rồi ack một seq sau với `AckAll`, message bị skip sẽ **mất vĩnh viễn** từ góc nhìn consumer.

### 1.3 Double-Ack — chìa khóa cho exactly-once consumption

Một `Ack()` thường là fire-and-forget — nếu network drop ack, server sẽ redeliver. **`AckSync()` / `double_ack()`** đặt reply subject trên ack message và chờ server confirm. Trích docs chính thức:

> *"If the response received from the server indicates success you can be sure that the message will never be re-delivered by the consumer (due to a loss of your acknowledgement)."*

Đây là **bắt buộc** cho exactly-once consumption. Combo canonical: **pull consumer + AckExplicit + double_ack**.

### 1.4 Headers điều khiển concurrency tối ưu

| Header | Mục đích |
|---|---|
| `Nats-Msg-Id` | Khóa dedup trong `duplicate_window` |
| `Nats-Expected-Stream` | Reject nếu message đến nhầm stream |
| `Nats-Expected-Last-Msg-Id` | CAS theo last `Nats-Msg-Id` |
| `Nats-Expected-Last-Sequence` | CAS theo last seq (toàn stream) |
| `Nats-Expected-Last-Subject-Sequence` | CAS theo seq của subject (event sourcing!) |
| `Nats-Rollup` | Purge `all` / `sub` trước khi insert |
| `Nats-TTL` | TTL per-message (NATS 2.11+) |

CAS thất bại trả về `wrong last sequence: <n>` và message bị reject — đây là cơ chế **optimistic concurrency control** cho event sourcing trên JetStream.

### 1.5 Synadia nói gì về "exactly-once"?

Trích Byron Ruth, NATS Weekly #33:

> *"Đạt được exactly-once **delivery** trên mạng là **bất khả thi** ở mức kỹ thuật. Đã được nghiên cứu, thảo luận đến mức nhàm chán và chứng minh là không thể. Tuy nhiên, hầu hết khi nói về exactly-once, người ta không quan tâm *làm sao* đạt được, mà là kết quả quan sát được từ góc nhìn user/business."*

Và NATS Weekly #3:

> *"NATS hỗ trợ **partial** exactly-once delivery thông qua dedup messages kết hợp double ack. Tôi nói partial vì về mặt lý thuyết không thể đảm bảo trong mọi failure case. Exactly-once **processing** thường khả thi hơn vì nó liên quan đến phía consumer."*

Đây là **honesty-by-design**: at-least-once + idempotency là cách duy nhất để chuyển đổi thành exactly-once *quan sát được*.

### 1.6 Các kịch bản failure mà message vẫn có thể bị mất hoặc trùng

**Dedup window hết hạn trước khi retry.** Publisher crash >2 phút (default), retry với cùng `Nats-Msg-Id` → server đã evict entry → message append lại → consumer thấy duplicate. **Mitigation:** tăng `duplicate_window` hoặc dùng `DiscardNewPerSubject` + ID-trong-subject cho dedup vô hạn.

**Leader election trong R3/R5.** Mỗi stream là một Raft group riêng. Khi leader mất, API trả `503 no responders` cho đến khi có leader mới (ADR-22 khuyến nghị client retry với backoff). Writes đã ack chỉ an toàn **nếu** đã đạt quorum-committed Raft entry **và** đã `fsync` trên quorum.

**Báo cáo Jepsen 2025 trên NATS 2.12.1** (jepsen.io/analyses/nats-2.12.1) là một wake-up call quan trọng:
- File corruption (single bit flip) ở `.blk` files trên một minority node có thể gây **persistent split-brain**, các node trả về message sets khác nhau (issue #7549).
- Snapshot corruption khiến leader tuyên bố stream "orphaned" và **xoá toàn cluster** (issue #7556).
- **Mất điện đồng thời** nhiều node ở config mặc định → mất **131,418 / 930,005 messages đã ack (~14.1%)**.
- Trong 2.10.22, process crash đơn lẻ có thể xoá toàn bộ stream (#6888, đã fix ở 2.10.23).

**Hành vi `fsync` mặc định.** `jetstream.sync_interval = 2m` (mặc định). JetStream **ack publishes TRƯỚC khi fsync**, dựa vào replication. Synadia đã update docs (Nov 2025): *"Nếu chạy JetStream không replication hoặc R2, bạn có thể muốn rút ngắn fsync interval."* Đặt `sync_interval: always` cho durability tối đa nhưng throughput rớt còn "vài trăm msg/s." **Khuyến nghị:** với business-critical events, dùng R3 tối thiểu, cân nhắc `sync_interval: 10s` hoặc thấp hơn nếu rate cho phép.

**Consumer crash sau khi xử lý nhưng trước khi ack.** Side-effect đã thực hiện nhưng ack không gửi → AckWait expire → redelivery → side-effect lần hai. **`AckSync` không cứu được trường hợp này** — chỉ idempotent processing (inbox table) hoặc atomic side-effect+ack mới giải quyết.

**Publisher crash trước khi nhận PubAck.** Không biết server có lưu hay chưa. Không có `Nats-Msg-Id` → retry sẽ tạo duplicate. Có `Nats-Msg-Id` và retry trong window → server trả `duplicate: true`.

### 1.7 Replication R1 vs R3 vs R5

| Replicas | Quorum | Chịu mất | Ghi chú |
|---|---|---|---|
| R1 | 1 | 0 node | Hiệu năng cao nhất, không fault-tolerance |
| **R3** | 2 | 1 node | **Tối thiểu cho production** |
| R5 | 3 | 2 node | Diminishing returns, write latency cao hơn |
| R2/R4 | 2/3 | 0/1 node | **Tránh** — Raft majority y hệt số lẻ thấp hơn |

Tối đa 5 replica. Mỗi stream là một Raft group; mỗi consumer cũng có Raft group riêng đặt cùng vị trí với stream replicas. Storage `file` (block files trong `jetstream.store_dir`, **tránh NFS/NAS**) hoặc `memory`. Per-message overhead trên file storage ≈ 39 bytes cho payload 5 bytes không headers (4 rec_len + 8 seq + 8 ts + 2 subj_len + subj + msg + 8 hash).

### 1.8 Retention policies

**`LimitsPolicy`** (mặc định): giữ đến khi vượt MaxMsgs/MaxBytes/MaxAge/MaxMsgsPerSubject. **`WorkQueuePolicy`**: xoá message khi consumer ack — chỉ một consumer non-overlapping per subject. **`InterestPolicy`**: giữ khi ít nhất một consumer chưa ack; không có consumer thì không lưu.

---

## 2. Pattern production-grade — không bao giờ mất event

### 2.1 Transactional Outbox — lời giải cho "dual-write problem"

**Vấn đề dual-write:** Service phải làm hai việc atomically: (1) commit DB business state, (2) publish event. Không có 2PC khả dụng. Hai thứ tự đều fail:

- Publish trước, commit sau → broker có phantom event của state chưa persist.
- Commit trước, publish sau → service crash giữa chừng → downstream không bao giờ biết.

**Lời giải:** đặt event như **dữ liệu**, ghi vào outbox table cùng transaction với business write, rồi publish bất đồng bộ.

```sql
CREATE TABLE outbox (
    id              UUID         PRIMARY KEY,           -- UUIDv7, dùng làm Nats-Msg-Id
    aggregate_type  TEXT         NOT NULL,
    aggregate_id    TEXT         NOT NULL,
    subject         TEXT         NOT NULL,
    payload         JSONB        NOT NULL,
    headers         JSONB        NOT NULL DEFAULT '{}',
    created_at      TIMESTAMPTZ  NOT NULL DEFAULT now(),
    published_at    TIMESTAMPTZ  NULL,
    attempts        INT          NOT NULL DEFAULT 0,
    last_error      TEXT         NULL
);
CREATE INDEX outbox_pending_idx ON outbox (created_at) WHERE published_at IS NULL;
```

**Polling worker với `SELECT ... FOR UPDATE SKIP LOCKED`** (PostgreSQL 9.5+, MySQL 8+, Oracle): cho phép nhiều worker chạy song song không tranh chấp. Worker crash giữa batch → transaction rollback → row trở về "pending."

Khi publish, dùng outbox row ID làm `Nats-Msg-Id`: nếu worker crash giữa "publish" và "UPDATE published_at", việc publish lại sẽ **bị JetStream dedup** (nếu trong window). Đây là combo hoàn hảo.

**Tradeoff:** latency 50ms–1s, DB load tăng (cần partial index, partitioning ở scale lớn). Pitfall: replay sau khi window hết hạn → cần idempotency consumer-side cho long-tail.

### 2.2 CDC với Debezium → JetStream

**Architecture:** Debezium đọc Postgres WAL qua logical replication slot, decode INSERT/UPDATE/DELETE row events và emit. Hai topology:

1. **Debezium Server với native NATS JetStream sink** (`debezium-server-nats-jetstream`, hardened auth + TLS từ 2.7). Quarkus single-process, không cần Kafka.
2. **Debezium → Kafka → NATS bridge** (`nats-kafka` connector). Phức tạp hơn nhưng được hệ sinh thái Connect.

**Outbox + CDC kết hợp** (canonical pattern, Gunnar Morling 2019): insert vào outbox, Debezium capture INSERT events qua **Outbox Event Router SMT**, route đến NATS subjects với `aggregate_id` làm routing key.

**Pros vs polling:** latency thấp hơn (millisecond log-tailing), không có DB poll load, ordering miễn phí từ WAL. **Cons:** ops phức tạp (replication slot, slot bloat khi connector chậm), schema evolution challenges, TOAST column handling.

### 2.3 Inbox pattern — idempotency phía consumer

```sql
CREATE TABLE inbox (
    message_id    TEXT         NOT NULL,
    consumer      TEXT         NOT NULL,
    processed_at  TIMESTAMPTZ  NOT NULL DEFAULT now(),
    result        JSONB        NULL,
    PRIMARY KEY (consumer, message_id)
);
```

**Atomic process + record trong một DB transaction:**

```sql
BEGIN;
  INSERT INTO inbox(message_id, consumer) VALUES ($1, 'order-svc')
    ON CONFLICT DO NOTHING RETURNING message_id;
  -- nếu rows = 0 → duplicate, ack rồi return
  -- nếu rows = 1 → thực hiện business write trong cùng tx
  UPDATE inventory SET reserved = reserved + $qty WHERE sku = $sku;
COMMIT;
-- chỉ sau khi commit thành công: msg.double_ack()
```

Đây là Pat Helland's "remember the message has been processed" idempotence (CIDR 2007). Cleanup: `DELETE FROM inbox WHERE processed_at < now() - INTERVAL '7 days'`. **TTL phải vượt quá max retry window** (AckWait × MaxDeliver + replay budget).

**"Listen to Yourself" + Outbox + Inbox** (Wade/Confluent, event-driven.io): trong cùng DB transaction — đọc message → check inbox → nếu mới, insert inbox + apply state + insert outbox → commit → ack. Outbox chống mất, inbox chống trùng.

### 2.4 Saga pattern — transactions phân tán không có 2PC

Saga = chuỗi local ACID transactions T1...Tn, mỗi step emit event/message kích hoạt step kế tiếp. Nếu Tk fail → compensating transactions C(k-1)...C1 logically undo. **Compensations là semantic undos**, không phải rollback (thế giới đã chuyển động: email đã gửi, API đã call) — phải **idempotent** và **retryable**.

| Choreography | Orchestration |
|---|---|
| Services publish events, peers react | Orchestrator phát commands, consume replies |
| ≤3-4 participants, flow đơn giản | Flow phức tạp/branching, regulated |
| Low coupling, không cần infra orchestrator | Explicit state, dễ observability |
| Hidden saga state, khó debug | Risk: domain logic leak vào orchestrator |

**Frameworks:** Temporal/Cadence (workflow-as-code, durable execution, ANZ Bank/Maersk dùng), Camunda/Zeebe (BPMN), AWS Step Functions, Eventuate Tram. **Trong Rust hiện chưa có saga framework nào tương đương MassTransit/NServiceBus** — phải tự build state machine consume qua trait `Subscriber`.

### 2.5 DLQ trong JetStream

JetStream **không có built-in DLQ** truyền thống vì stream phục vụ nhiều consumers. Thay vào đó dùng **advisories**:

| Event | Subject |
|---|---|
| Max deliveries reached | `$JS.EVENT.ADVISORY.CONSUMER.MAX_DELIVERIES.<STREAM>.<CONSUMER>` |
| Message terminated (`AckTerm`) | `$JS.EVENT.ADVISORY.CONSUMER.MSG_TERMINATED.<STREAM>.<CONSUMER>` |

**Hai pattern thực thi:**

1. **DLQ stream listening advisories:** tạo stream `DLQ` subscribe các advisory subjects. Worker đọc DLQ, parse `stream_seq`, fetch message gốc qua `nats stream get`. Chỉ work với `LimitsPolicy`.
2. **Application-level DLQ:** worker check `msg.info().delivered`, nếu ≥ threshold thì republish đến `DLQ.orders` stream + ack original. **Set `MaxDeliver` cao hơn app threshold** để redelivery không race với app.

**Term** rõ ràng: khi xác định lỗi vĩnh viễn (malformed payload, schema mismatch, business validation fail) → `msg.ack_with(AckKind::Term)` ngay, đừng để retry storm.

### 2.6 Retry với exponential backoff — full jitter

Marc Brooker (AWS Architecture Blog) chứng minh thực nghiệm: no-jitter là tệ nhất; full jitter và decorrelated jitter giảm completion time + total client work >50%. **Luôn cap, luôn jitter.**

```
# Full Jitter (mặc định khuyến nghị, AWS Java SDK)
sleep = random_uniform(0, min(cap, base * 2^attempt))

# Decorrelated Jitter (chống thundering-herd ở scale lớn)
sleep = min(cap, random_uniform(base, prev_sleep * 3))
```

NATS-specific: `msg.ack_with(AckKind::Nak(Some(delay)))` cho per-message backoff; consumer config field `BackOff []Duration` (e.g., `[1s, 5s, 30s, 5m, 30m]`) ghi đè AckWait theo delivery count. Cho long delay (giờ/ngày), republish sang "delay" stream với `Nats-Schedule-Next-Time` (NATS 2.12+) thay vì giữ AckWait.

### 2.7 Idempotency keys — chiến lược Message ID

| Strategy | Tính chất | Khi nào dùng |
|---|---|---|
| **UUIDv7** (RFC 9562, 2024) | 48-bit ms timestamp + 74-bit random | **Mặc định cho hệ thống mới** — B-tree friendly, Buildkite báo giảm 50% WAL rate, insert nhanh 2-5× so với UUIDv4 |
| UUIDv4 | 122-bit random | Privacy tokens; tệ làm DB PK (B-tree fragmentation) |
| ULID | 48-bit ts + 80-bit random, base32-sortable | Khi cần lex-sort |
| Business key + timestamp | `order-{id}-{epoch}` | Identity tự nhiên rõ |
| Content hash (SHA-256) | Deterministic dedup theo content | Upstream emit cùng event 2 lần, collision risk ~2^-128 |
| CDC LSN tuple `{commit_lsn, event_lsn}` | Monotonic per Postgres | Khi dùng Debezium |

Phân biệt (Morling): **deduplication keys** ngăn unit-of-work chạy 2 lần; **idempotency keys** đảm bảo *nếu* chạy 2 lần thì final state vẫn đúng. Nhiều hệ thống cần cả hai.

**Storage backends:**

- **Redis** `SET key val NX EX ttl` — nhanh, in-memory; rủi ro AOF gap, eviction.
- **PostgreSQL unique constraint** + `INSERT ... ON CONFLICT DO NOTHING` — atomic nhất, transactional với business write, chậm nhất.
- **NATS KV store** — built-on JetStream, cùng infra, atomic `Create` (true SETNX), TTL native, replicate cùng cluster. **Lựa chọn ưu việt cho NATS-only stack** — không cần Redis riêng.

**Hard rule TTL:** TTL > (max producer retry window + max consumer redelivery window + clock skew). Inbox/KV ≥ 24h–7d nếu có cross-region replay hoặc operator manual replay.

**Race conditions:** ❌ check-then-act sẽ bị 2 worker pass; ✅ atomic upsert (`ON CONFLICT DO NOTHING`, `SET NX`, `KV.Create`) — single round-trip, single decision.

### 2.8 Event sourcing như nền tảng

Lưu **append-only sequence of facts** thay vì current state. Replay = re-derivation, không phải duplicate side-effect — phù hợp tự nhiên với at-least-once. Components: event log, projections (idempotent consumers fold events thành read models), snapshots (rebuild từ seq S thay vì 0). **JetStream làm event store khả thi:** per-aggregate streams qua subject hierarchy (`orders.<id>.events`), `MaxMsgsPerSubject` + `Nats-Expected-Last-Subject-Sequence` cho compare-and-append (optimistic concurrency).

**Cẩn trọng:** event sourcing không phải silver bullet — schema evolution phức tạp (events là forever, cần versioning), storage tăng, coupling chuyển sang event payload. Chỉ dùng khi audit/temporal queries/rebuild-any-view bù được chi phí.

---

## 3. Implementation Rust — code production-grade

### 3.1 Cargo.toml baseline

```toml
[dependencies]
async-nats = "0.46"
tokio       = { version = "1.36", features = ["full"] }
futures     = "0.3"
futures-util = "0.3"
bytes       = "1.5"
serde       = { version = "1", features = ["derive"] }
serde_json  = "1"
thiserror   = "1.0"
tracing     = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
uuid        = { version = "1", features = ["v7", "serde"] }
sqlx        = { version = "0.8", features = ["runtime-tokio", "tls-rustls-ring-webpki", "postgres", "uuid", "chrono", "json"] }
async-trait = "0.1"
opentelemetry = "0.24"

[dev-dependencies]
testcontainers          = "0.23"
testcontainers-modules  = { version = "0.11", features = ["nats"] }
proptest                = "1"
mockall                 = "0.13"
```

**Lưu ý:** `async-nats 0.46` (April 2026) là client chính thức duy nhất được maintain bởi Synadia. Crate `nats` (sync) đã deprecated từ 0.26.0.

### 3.2 Producer với deduplication

```rust
use async_nats::{HeaderMap, jetstream};
use bytes::Bytes;
use uuid::Uuid;

#[derive(Clone)]
pub struct Producer {
    js: jetstream::Context,
}

impl Producer {
    pub async fn connect(url: &str) -> Result<Self, async_nats::Error> {
        let client = async_nats::connect(url).await?;
        Ok(Self { js: jetstream::new(client) })
    }

    /// Publish idempotent với explicit Msg-Id (path khuyến nghị).
    pub async fn publish_idempotent(
        &self,
        subject: &str,
        msg_id: &str,
        payload: Bytes,
    ) -> Result<jetstream::context::PublishAck, async_nats::Error> {
        let mut headers = HeaderMap::new();
        headers.insert("Nats-Msg-Id", msg_id);

        // Await đầu: request rời client.
        // Await thứ hai: PubAck từ server.
        let ack = self.js
            .publish_with_headers(subject.to_owned(), headers, payload)
            .await?
            .await?;

        if ack.duplicate {
            tracing::info!(msg_id, subject, "duplicate suppressed by JetStream");
        }
        Ok(ack)
    }
}
```

**Pitfall quan trọng:** khi `ack.duplicate == true`, message **KHÔNG** có trong stream (đã bị suppress). Nhiều integration quên check field này và double-count published events.

### 3.3 Tạo stream với R3, dedup window 5 phút

```rust
use async_nats::jetstream::stream::{Config, RetentionPolicy, StorageType, DiscardPolicy};
use std::time::Duration;

pub async fn ensure_stream(js: &jetstream::Context) -> Result<(), async_nats::Error> {
    js.get_or_create_stream(Config {
        name:               "EVENTS".into(),
        subjects:           vec!["events.>".into()],
        retention:          RetentionPolicy::Limits,
        storage:            StorageType::File,
        num_replicas:       3,                                  // R3 production-min
        duplicate_window:   Duration::from_secs(5 * 60),        // 5 phút
        discard:            DiscardPolicy::Old,
        max_age:            Duration::from_secs(7 * 24 * 3600), // 7 ngày
        allow_direct:       true,
        ..Default::default()
    }).await?;
    Ok(())
}
```

### 3.4 Pull consumer với double-ack và idempotency

```rust
use async_nats::jetstream::{
    self,
    consumer::{pull, AckPolicy, DeliverPolicy},
    AckKind, Message,
};
use futures_util::StreamExt;

pub async fn run_consumer(js: jetstream::Context) -> Result<(), async_nats::Error> {
    let stream = js.get_stream("EVENTS").await?;
    let consumer = stream
        .get_or_create_consumer("orders-worker", pull::Config {
            durable_name:    Some("orders-worker".into()),
            filter_subject:  "events.orders.>".into(),
            ack_policy:      AckPolicy::Explicit,
            deliver_policy:  DeliverPolicy::All,
            ack_wait:        std::time::Duration::from_secs(30),
            max_deliver:     5,
            ..Default::default()
        }).await?;

    let mut messages = consumer.messages().await?;
    while let Some(item) = messages.next().await {
        let msg = match item { Ok(m) => m, Err(e) => { tracing::warn!(?e); continue; } };

        let info = msg.info()?;
        let msg_id = msg.headers.as_ref()
            .and_then(|h| h.get("Nats-Msg-Id"))
            .map(|v| v.as_str().to_owned())
            .unwrap_or_else(|| info.stream_sequence.to_string());

        match handle(&msg_id, &msg).await {
            Ok(()) => {
                // double_ack chờ server confirm → exactly-once consumption
                msg.double_ack().await?;
            }
            Err(HandlerError::Permanent(_)) => {
                // Không retry, gửi DLQ
                msg.ack_with(AckKind::Term).await?;
            }
            Err(HandlerError::Transient(_)) => {
                msg.ack_with(AckKind::Nak(Some(std::time::Duration::from_secs(5)))).await?;
            }
        }
    }
    Ok(())
}

#[derive(thiserror::Error, Debug)]
enum HandlerError {
    #[error("permanent: {0}")] Permanent(String),
    #[error("transient: {0}")] Transient(String),
}

async fn handle(_msg_id: &str, _msg: &Message) -> Result<(), HandlerError> { Ok(()) }
```

### 3.5 Outbox pattern hoàn chỉnh với sqlx + async-nats

```rust
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;
use serde::Serialize;
use bytes::Bytes;
use std::time::Duration;

#[derive(Serialize)]
struct OrderCreated { order_id: Uuid, total: i64 }

/// Atomic insert: business write + outbox row trong cùng transaction.
pub async fn create_order(pool: &PgPool, total: i64) -> Result<Uuid, sqlx::Error> {
    let mut tx: Transaction<'_, Postgres> = pool.begin().await?;
    let order_id = Uuid::now_v7();

    sqlx::query!(
        "INSERT INTO orders (id, total) VALUES ($1, $2)",
        order_id, total
    ).execute(&mut *tx).await?;

    let event = OrderCreated { order_id, total };
    sqlx::query!(
        r#"INSERT INTO outbox (id, aggregate_type, subject, payload)
           VALUES ($1, 'order', $2, $3)"#,
        Uuid::now_v7(),
        format!("events.orders.{}.created", order_id),
        serde_json::to_value(&event).unwrap()
    ).execute(&mut *tx).await?;

    tx.commit().await?;
    Ok(order_id)
}

/// Dispatcher: poll, FOR UPDATE SKIP LOCKED, publish, mark sent.
/// Nats-Msg-Id = outbox.id → re-runs sau crash không bao giờ duplicate.
pub async fn dispatch_loop(
    pool: PgPool,
    js: async_nats::jetstream::Context,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    loop {
        let mut tx = pool.begin().await?;
        let rows = sqlx::query!(
            r#"SELECT id, subject, payload
               FROM outbox
               WHERE published_at IS NULL
               ORDER BY created_at
               FOR UPDATE SKIP LOCKED
               LIMIT 100"#
        ).fetch_all(&mut *tx).await?;

        if rows.is_empty() {
            tx.commit().await?;
            tokio::time::sleep(Duration::from_millis(250)).await;
            continue;
        }

        for r in rows {
            let payload = Bytes::from(serde_json::to_vec(&r.payload).unwrap());
            let mut h = async_nats::HeaderMap::new();
            h.insert("Nats-Msg-Id", r.id.to_string().as_str()); // dedup key

            match js.publish_with_headers(r.subject.clone(), h, payload).await {
                Ok(fut) => match fut.await {
                    Ok(_ack) => {
                        sqlx::query!(
                            "UPDATE outbox SET published_at = now() WHERE id = $1",
                            r.id
                        ).execute(&mut *tx).await?;
                    }
                    Err(e) => {
                        sqlx::query!(
                            "UPDATE outbox SET attempts = attempts+1, last_error=$2 WHERE id=$1",
                            r.id, e.to_string()
                        ).execute(&mut *tx).await?;
                    }
                },
                Err(e) => {
                    sqlx::query!(
                        "UPDATE outbox SET attempts = attempts+1, last_error=$2 WHERE id=$1",
                        r.id, e.to_string()
                    ).execute(&mut *tx).await?;
                }
            }
        }
        tx.commit().await?;
    }
}
```

**Tính chất correctness:**
- Atomic insert: event tồn tại iff business write commit.
- `FOR UPDATE SKIP LOCKED` cho phép horizontal scale dispatcher.
- `Nats-Msg-Id = outbox.id` → redrive trong dedup window là no-op; ngoài window dựa vào consumer-side idempotency.
- UUIDv7 → globally unique, monotonic, B-tree friendly.

### 3.6 Idempotent handler với NATS KV store

```rust
use async_nats::jetstream::{self, kv};
use bytes::Bytes;
use std::time::Duration;

pub async fn ensure_idempotency_kv(
    js: &jetstream::Context,
) -> Result<kv::Store, async_nats::Error> {
    js.create_key_value(kv::Config {
        bucket:        "processed".into(),
        history:        1,
        max_age:        Duration::from_secs(24 * 3600),
        num_replicas:   3,
        ..Default::default()
    }).await
}

/// Trả Ok(true) nếu work thực sự chạy, Ok(false) nếu đã processed trước đó.
pub async fn handle_idempotent<F, Fut, T, E>(
    kv:    &kv::Store,
    msg_id: &str,
    work:   F,
) -> Result<bool, async_nats::Error>
where
    F:   FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
    E:   std::fmt::Display,
{
    // Atomic create: thành công duy nhất lần đầu thấy id này.
    match kv.create(msg_id, Bytes::from_static(b"pending")).await {
        Ok(rev) => {
            match work().await {
                Ok(_) => {
                    kv.update(msg_id, Bytes::from_static(b"done"), rev).await?;
                    Ok(true)
                }
                Err(e) => {
                    tracing::warn!(%msg_id, error = %e, "handler failed; pending sẽ expire");
                    Err(async_nats::Error::from(std::io::Error::other(e.to_string())))
                }
            }
        }
        Err(_e) => Ok(false), // key đã tồn tại
    }
}
```

KV `Create` là atomic compare-and-set (fail nếu key tồn tại). KV được build trên stream `KV_<bucket>` → có replication, history, watching, TTL miễn phí.

### 3.7 Thiết kế API public — phong cách Azure Service Bus

```rust
use async_trait::async_trait;
use bytes::Bytes;
use serde::{de::DeserializeOwned, Serialize};
use std::{borrow::Cow, time::Duration};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum BusError {
    #[error("nats: {0}")]      Nats(#[from] async_nats::Error),
    #[error("publish: {0}")]   Publish(String),
    #[error("ack: {0}")]       Ack(String),
    #[error("serde: {0}")]     Serde(#[from] serde_json::Error),
    #[error("idempotency store: {0}")] Idempotency(Cow<'static, str>),
    #[error("permanent handler error: {0}")] Permanent(String),
    #[error("transient handler error: {0}")] Transient(String),
}
pub type BusResult<T> = Result<T, BusError>;

pub trait Event: Serialize + DeserializeOwned + Send + Sync + 'static {
    fn subject(&self) -> Cow<'_, str>;
    fn message_id(&self) -> Uuid;
}

#[async_trait]
pub trait Publisher: Send + Sync {
    async fn publish<E: Event>(&self, evt: &E) -> BusResult<PubReceipt>;
}

#[derive(Debug, Clone)]
pub struct PubReceipt {
    pub stream:    String,
    pub sequence:  u64,
    pub duplicate: bool,
}

#[async_trait]
pub trait EventHandler<E: Event>: Send + Sync + 'static {
    async fn handle(&self, ctx: &HandlerCtx, evt: E) -> BusResult<()>;
}

pub struct HandlerCtx {
    pub msg_id:     Uuid,
    pub stream_seq: u64,
    pub delivered:  u64,
    pub trace:      tracing::Span,
}

#[derive(Clone, Debug)]
pub struct SubscribeOptions {
    pub durable:      String,
    pub filter:       String,
    pub max_deliver:  i64,
    pub ack_wait:     Duration,
    pub concurrency:  usize,
    pub idempotency:  IdempotencyMode,
    pub dlq_subject:  Option<String>,
}

#[derive(Clone, Debug)]
pub enum IdempotencyMode {
    Off,
    NatsKv { bucket: String, ttl: Duration },
    Postgres { pool: sqlx::PgPool, table: &'static str },
}

#[async_trait]
pub trait Subscriber: Send + Sync {
    async fn subscribe<E, H>(&self, opts: SubscribeOptions, handler: H) -> BusResult<SubscriptionHandle>
    where E: Event, H: EventHandler<E>;
}

pub struct SubscriptionHandle { /* JoinHandle inside */ }
```

Builder pattern cho ergonomics:

```rust
pub struct EventBusBuilder { /* ... */ }

impl EventBusBuilder {
    pub fn new() -> Self { /* sane defaults: R3, 5min dedup */ todo!() }
    pub fn url(self, u: impl Into<String>) -> Self { todo!() }
    pub fn replicas(self, n: usize) -> Self { todo!() }
    pub fn dedup_window(self, d: Duration) -> Self { todo!() }
    pub fn idempotency(self, i: IdempotencyMode) -> Self { todo!() }
    pub async fn build(self) -> BusResult<EventBus> { todo!() }
}
```

### 3.8 Hệ sinh thái Rust hiện có (April 2026)

| Crate | Mục đích | Đánh giá |
|---|---|---|
| **`async-nats` 0.46** | Official NATS/JetStream client | Active, Synadia maintain |
| `nats` (sync) | Legacy | **Deprecated** |
| `oxide-outbox` (Vancoola) | Async transactional outbox: `outbox-core`, `outbox-postgres`, `outbox-redis` | Mới, hỗ trợ UUIDv7, NOTIFY-based wakeup |
| `axum-idempotent` | Middleware Tower/Axum cache responses theo `Idempotency-Key` | Hữu ích cho HTTP edge |
| `cqrs-es` / `eventually-rs` / `event_sourcing.rs` (primait) | CQRS / Event Sourcing | Production-ready |
| `eventastic` | Fork eventually-rs forced transactions + idempotency | Smaller |
| **Saga frameworks** | Không có Rust framework production-grade tương đương MassTransit/NServiceBus | **Khoảng trống — cơ hội cho thư viện mới** |

**Không có crate nào** hiện bundling "JetStream + idempotency + outbox + Service-Bus-style API" → thư viện đề xuất của user **lấp đúng gap thật**.

### 3.9 Testing với testcontainers + chaos

```rust
#[tokio::test]
async fn publishes_with_dedup() -> anyhow::Result<()> {
    use testcontainers::{runners::AsyncRunner, ImageExt, GenericImage};
    use testcontainers::core::{IntoContainerPort, WaitFor};

    let nats = GenericImage::new("nats", "2.12-alpine")
        .with_exposed_port(4222.tcp())
        .with_wait_for(WaitFor::message_on_stderr("Server is ready"))
        .with_cmd(["-js"])
        .start().await?;

    let host = nats.get_host().await?;
    let port = nats.get_host_port_ipv4(4222).await?;
    let url  = format!("nats://{host}:{port}");

    let producer = evtbus::Producer::connect(&url).await?;
    evtbus::ensure_stream(&producer.js()).await?;

    let id = uuid::Uuid::now_v7().to_string();
    let a = producer.publish_idempotent("events.test", &id, "x".into()).await?;
    let b = producer.publish_idempotent("events.test", &id, "x".into()).await?;

    assert_eq!(a.duplicate, false);
    assert_eq!(b.duplicate, true);   // server-side dedup proven
    Ok(())
}
```

Property-based testing với `proptest` cho idempotent handler — chạy sequence với duplicates, verify counter = số unique IDs.

Chaos testing với **toxiproxy-rust** trong front của NATS để inject latency/timeouts/drops. **Antithesis** (deterministic simulation, partner của Synadia) đã catch nhiều JS bug — recommended cho thư viện business-critical.

---

## 4. Fallback khi NATS không khả dụng

### 4.1 Local persistent queue làm buffer

Khi broker không reachable, publisher có 3 lựa chọn: (a) block, (b) drop, (c) buffer cục bộ. Pattern dominant cho "never lose" là **durable buffer**.

- **SQLite WAL mode** với UNIQUE `idempotency_key` index — `litequeue`, `liteq` đều dùng pattern này. Đơn giản, ACID, single-file. Không phù hợp >10K writes/sec/proc do single-writer lock.
- **sled / RocksDB** khi vượt SQLite — LSM, hàng trăm nghìn writes/sec.
- **Outbox pattern** (đã trình bày) — *the canonical solution* khi đã có DB.

Khi NATS phục hồi: relay đọc theo monotonic order, publish với `Nats-Msg-Id` (JetStream dedup window dedup retries trong window), mark `delivered=true` chỉ sau PubAck. Bounded backoff, respect `MaxAckPending`. Cho long outage, batch async publish.

**MachineMetrics và PowerFlex** (Synadia case studies) explicit dùng pattern này — JetStream trên edge devices cho local persistence, auto-sync khi connectivity trở lại.

### 4.2 Dual-write — anti-pattern phải tránh

Ghi DB + broker không có boundary là classic anti-pattern. Failure modes (Confluent, Kurrent):
- DB commit OK, broker fail → event lost.
- Broker OK, DB rollback → phantom event.
- Cả hai OK nhưng consumer nhận thứ tự inconsistent.

Lời giải đã trình bày: outbox + relay/poll, CDC (Debezium), event sourcing, listen-to-yourself.

### 4.3 Circuit breaker, bulkhead, graceful degradation

**Circuit breaker** (Hystrix/resilience4j) áp dụng cho broker: track publish failure rate qua sliding window (e.g., `failureRateThreshold=50%, slidingWindowSize=10, waitDurationInOpenState=5s`). OPEN → route ngay vào local outbox; HALF_OPEN trial probe check recovery. **Bulkhead:** isolate publish thread pool — slow NATS publisher không exhaust app threads. Trong Rust idiomatic là bounded `tokio::sync::Semaphore` + dedicated task.

**Quyết định graceful degradation theo criticality:**

| Criticality | Strategy khi NATS down |
|---|---|
| Telemetry, metrics, traces | Drop với counter increment ("fail soft") |
| User-facing async (notifications) | SQLite buffer cục bộ, replay |
| **Financial / audit / order events** | **Outbox transactional DB; reject request nếu outbox write fail ("fail loud")** |
| Real-time control loops | Switch mode degraded local-only |

**Heuristic:** fail loud khi user có response actionable (HTTP 503 retry) và cost của phantom success cao (payment, order). Buffer locally khi producer autonomous (IoT gateway, edge), retry unattended, disk capacity vượt outage window. Form3 chọn buffer-locally + active-active multi-cloud cho payments vì SLA 500ms không cho phép fail-loud.

### 4.4 NATS + Kafka hybrid

**Architecture:**
- **NATS (Core + JetStream)** cho: ephemeral pub/sub, request-reply RPC, microservice fan-out, edge-to-cloud, multi-tenant subjects, command/control.
- **Kafka** cho: durable analytics log (months-years), Kafka Connect ecosystem (200+ connectors), Streams/ksqlDB, Hadoop/Spark/Flink integration.

**`nats-kafka` bridge** (github.com/nats-io/nats-kafka): one process, multiple unidirectional connectors. Mapping Kafka topic ↔ NATS subject. Confluent Schema Registry support. **Caveat:** queue groups không preserve ordering, hai bridge cùng queue có thể out-of-order Kafka writes.

**Khi nào hybrid hợp lý:** đã có Kafka analytics estate nhưng cần microsecond latency operational; cần Kafka Connect connectors nhưng Kafka p99 10-50ms unacceptable; edge → cloud (NATS edge, Kafka data lake).

**Tradeoff:** ops kép, JVM tuning + partition sizing + Connect cluster + JetStream Raft. Synadia whitepaper ghi rõ Kafka cần "rất nhiều configuration và tuning, nhiều timing settings" trong khi NATS "comparatively simpler hơn nhiều."

**Khi JetStream alone đủ:** throughput < ~500K msg/s sustained, retention hours-weeks, latency SLA < 5ms p99, edge deployments, team không có JVM expertise. Form3, MachineMetrics, Intelecy, NVIDIA Cloud Functions tất cả NATS-only.

### 4.5 Disaster recovery

**Mirror** (1-to-1 read-only copy) — DR/geo-replication; **Source** (many-to-one aggregation) — analytics roll-ups. Cả hai async với 10-20s recovery interval. **Quan trọng:** không replicate deletes — không phải full sync.

Cross-region: **leaf nodes** + **JetStream domains** cho hub-and-spoke. Form3 dùng cho active-active multi-cloud — workloads "có thể shift dễ dàng từ cloud này sang cloud khác."

**Backup/restore:** `nats stream backup` (JSON config + optional `--data` tarball), `nats stream restore`. R1 chỉ restore từ snapshot; R3/R5 self-heal khi quorum survive. **Synadia Cloud delegate backup data cho customer.**

| Setup | RPO | RTO |
|---|---|---|
| R1 + sync_interval=2m | Tối đa 2 phút trên OS crash | Cho đến khi disk recover |
| R3 cluster, single AZ fail | 0 (Raft quorum) | Seconds (leader election) |
| R3 + cross-region mirror | 10-20s replication lag | Seconds, manual cutover |
| Active-active independent clusters | 0 (cả hai sites) | Near-zero |

---

## 5. So sánh với Kafka & Pulsar — quyết định khi nào đủ NATS

### 5.1 Kafka exactly-once

1. **Idempotent producer** (`enable.idempotence=true`, default từ Kafka 3.0): broker assign Producer ID, batch carry sequence number, broker reject duplicates trong partition.
2. **Transactions API** (`transactional.id`, `initTransactions`, `beginTransaction`, `sendOffsetsToTransaction`, `commitTransaction`): atomic across partitions trong **một** Kafka cluster, coordinated bởi transaction coordinator broker via 2PC, recorded trong `__transaction_state` topic.
3. **Consumer**: `isolation.level=read_committed` filter aborted messages, block past LSO (Last Stable Offset).
4. **Kafka Streams**: `processing.guarantee=exactly_once_v2`.

**Limit:** chỉ trong single Kafka cluster; transactional throughput cost (extra RPCs); LSO blocking thêm tail latency; không safe với FaaS / very-short-lived producers.

### 5.2 Pulsar exactly-once

1. **Broker deduplication** (`brokerDeduplicationEnabled=true`): producer specify `producerName` + `sendTimeout=0`, broker track `(producerName, sequenceId)`. Pulsar gọi là "effectively-once."
2. **Transactions** (GA từ 2.8): atomic produce + ack across topics/partitions via Transaction Coordinator.

### 5.3 Decision matrix

| Dimension | NATS JetStream | Kafka | Pulsar |
|---|---|---|---|
| Sustained throughput | 200K-400K msg/s persisted; millions cho Core | 500K-1M+ msg/s với batching | 1M+, tiered storage |
| p99 latency persisted | **1-5ms** | 10-50ms | 5-25ms |
| Exactly-once strength | Pub-side dedup window + ack; **không** multi-stream txn | **Strongest:** idempotent + transactions across partitions | Effectively-once dedup + transactions across topics |
| Ops complexity | **Lowest** (single 18MB Go binary) | High (broker tuning, Connect cluster) | **Highest** (Broker + BookKeeper + ZooKeeper) |
| Ecosystem | Growing (Benthos, KV, Object Store built-in) | **Largest** (Connect, Streams, ksqlDB, Schema Registry, Flink) | Solid (Pulsar IO, Functions) |
| Multi-region/edge | **Native** (leaf nodes, superclusters, mirror/source) | MirrorMaker2, Cluster Linking (Confluent paid) | Geo-replication built-in |
| Retention | Hours-weeks typical | Days-years; tiered storage | **Tiered storage to S3/GCS — strongest** |
| Resource footprint | **~18MB Go binary, edge-friendly** | JVM, 8+ cores, 64-128GB RAM | JVM + BookKeeper |
| Atomic txn across destinations | **Không** | **Yes** (within cluster) | **Yes** (within cluster) |

### 5.4 Khi nào NATS đủ vs khi cần Kafka/Pulsar

**Stay NATS JetStream khi:**
- Aggregate sustained throughput < ~500K msg/s.
- Retention needs hours-to-weeks.
- Latency SLA < 5ms p99.9 (real-time bidding, gaming, payments).
- Edge / constrained-resource deployments.
- Team prefer single-binary ops, lacks JVM expertise.

**Move to (or add) Kafka/Pulsar khi:**
- Sustained > 1M msg/s across well-partitioned topics.
- Long retention (months-years) cho reprocessing/compliance.
- Cần Kafka Streams / ksqlDB / Connect / Flink integration.
- Multi-system atomic transactions across many partitions là core business semantics.

### 5.5 Case studies sản xuất

**Tinder — "Project Keepalive"** (NATS Core): WebSocket "nudge" pipeline thay HTTP polling 2s. Latency từ 1.2s xuống 300ms (4× improvement).

**Form3** (UK fintech, payments rails SWIFT/BACS/SEPA): thay AWS SNS+SQS bằng NATS+JetStream. Lý do: AWS uptime 99.9% không đủ cho payments, cần multi-cloud active-active instant failover, SLA 500ms.

**Intelecy** (industrial IoT no-code AI): tens of thousands sensor endpoints feed Synadia Cloud. Stack K8s + Nomad + ClickHouse + gRPC + NATS + Kafka. **<2s round-trip** cho streaming data + ML insights.

**MachineMetrics** (industrial IoT, 1kHz machine data): migrating từ Amazon Kinesis sang NATS với leaf nodes; plan thêm JetStream trên edge cho persistent buffering trong network outage. Quote: *"NATS JetStream is a great connectivity replacement for SQLite."*

**NVIDIA Cloud Functions (NVCF)**: Synadia-managed NATS supercluster. JetStream durable work-queue decouple bursty HTTP/gRPC traffic from GPU capacity. Pull consumers + explicit acks → natural backpressure. Multi-region superclusters route đến nearest GPU fleet, mirrors cho regional failover.

**PowerFlex** (EV charging edge): NATS+JetStream "gần như eliminated data loss kể cả khi cellular drop ở môi trường khắc nghiệt như parking garages."

**Capital One** (engineering blog by Kevin Hoffman): promote NATS làm messaging substrate cloud-native microservices, contrast với operational weight của Kafka.

**Cảnh báo:** claim *"Walmart uses NATS"* xuất hiện trong marketing material nhưng tôi không tìm thấy primary engineering source confirm production use — flag là **chưa verified**.

**Tại sao một số công ty chọn Kafka:** LinkedIn/Uber/Netflix/Airbnb build infrastructure vào kỷ nguyên Kafka 2012-2018 khi alternatives chưa mature; cần Kafka Streams/Samza và Connect; throughput multi-million msg/s trên single topics (clickstream/ad-tech) vượt khả năng NATS Streaming (predecessor JetStream). Stripe (Confluent talks): Flink + Kafka cho CDC scale petabyte với multi-day replay.

---

## 6. Operational — monitoring và lỗi thường gặp

### 6.1 Metrics quan trọng

Từ `ConsumerInfo` JSON: `Delivered` (consumer_seq + stream_seq), `AckFloor`, `NumAckPending`, `NumRedelivered`, `NumWaiting`, `NumPending`. Từ stream state: `messages`, `bytes`, `first_seq`, `last_seq`, `consumer_count`, `num_deleted`.

**Prometheus exporter metrics chính** (chạy với `-jsz=all`):

```
jetstream_stream_total_messages
jetstream_stream_first_seq / last_seq
jetstream_consumer_delivered_consumer_seq / delivered_stream_seq
jetstream_consumer_ack_floor_consumer_seq / ack_floor_stream_seq
jetstream_consumer_num_ack_pending
jetstream_consumer_num_pending
jetstream_consumer_num_redelivered
jetstream_consumer_num_waiting
```

**Phát hiện dedup hits:** **không có metric dedicated** — chỉ qua `PubAck.Duplicate: true` ở client side. Client phải log để tổng hợp.

### 6.2 Advisory subjects critical phải subscribe

```
$JS.EVENT.ADVISORY.CONSUMER.MAX_DELIVERIES.<STREAM>.<CONSUMER>  → DLQ trigger
$JS.EVENT.ADVISORY.CONSUMER.MSG_TERMINATED.<STREAM>.<CONSUMER>  → explicit Term
$JS.EVENT.ADVISORY.STREAM.LEADER_ELECTED.<STREAM>               → leadership changes
$JS.EVENT.ADVISORY.CONSUMER.LEADER_ELECTED.<STREAM>.<CONSUMER>
```

**Quan trọng** (Todd Beets via Synadia Weekly #33): advisories chính chúng nó là **at-most-once** — DLQ pipeline thuần advisory có thể miss events; pair với stream-level checking.

### 6.3 Chaos testing approaches

- **toxiproxy** front cluster ports (4222, 6222, 7222) — inject latency/jitter/drop client↔server và server↔server.
- Kill node SIGKILL > SIGTERM — process crash; kết hợp packet loss test split-brain.
- **LazyFS** filesystem layer (Jepsen) — simulate power failure bằng drop non-fsynced writes.
- **Antithesis** deterministic simulation — Synadia partners; nhiều JS bugs (snapshot reorder #5700) caught qua đây.
- Run published `jepsen-io/nats` LXC test suite để reproduce 2025 findings.

### 6.4 10 lỗi phổ biến nhất khi implement exactly-once

1. **Quên `Nats-Msg-Id`.** Không có nó dedup là impossible. Producer retry sau timeout → duplicate stored. Dùng deterministic ID derived từ business key, **không phải** UUID generated tại publish time.
2. **AckWait quá ngắn.** Nếu `AckWait < p99 processing time` → server redeliver khi original đang xử lý → "redelivery storm" + saturate `MaxAckPending`. Rule: `AckWait ≥ p95(processing) × 2-3`, ưu tiên `AckProgress`/`+WPI` cho long jobs, dùng `BackOff` array spread retries.
3. **`MaxDeliver = -1` (unlimited).** Poison messages loop forever, block WorkQueue, consume `MaxAckPending` slots. Luôn set finite `MaxDeliver` + subscribe `MAX_DELIVERIES` advisories hoặc in-handler counter republish DLQ rồi ack.
4. **Async publish không check `PubAck`.** `js.PublishAsync` returns future; failure to await + inspect mỗi `PubAck` → silent loss/unnoticed duplicates. ADR-22 specify retry-on-503.
5. **Hiểu nhầm `AckAll`.** Không có nghĩa "đã processed all previous." Chỉ suppress redelivery. Skip một message do error rồi ack later seq với `AckAll` → message bị skip mất vĩnh viễn.
6. **Coi `PubAck` là durability.** Đến khi `sync_interval` fsync, R1 publish chỉ trong page cache. R1 strict durability cần `sync_interval: always`. R3/R5 trust quorum nhưng aware Jepsen 2025 đã chứng minh coordinated multi-node power loss vẫn lose data với default.
7. **Đọc `consumer info` cho pending count.** Per Synadia anti-patterns blog, trigger admin RPC mỗi call; dùng `msg.Metadata().NumPending` từ last delivered message thay thế.
8. **Even replica counts (R2, R4).** Không thêm fault tolerance so với số lẻ thấp hơn — Raft majority y hệt.
9. **Confusing `WorkQueue` overlapping consumers.** `WorkQueuePolicy` requires non-overlapping subject filters; ngược lại stream/consumer creation fail.
10. **Bỏ qua dedup window cho stream-to-stream sourcing** (issue #4459) — sourcing stream chứa duplicate `Nats-Msg-Id`s gây throttle ~2 messages per dedup window. Strip hoặc unique-ify msg-ids khi source.

### 6.5 Diagram — End-to-end flow exactly-once cho thư viện đề xuất

```
┌──────────────────┐    BEGIN tx                  ┌──────────────────┐
│   Application    │──┬── INSERT business ───────→│   PostgreSQL     │
│                  │  │                           │                  │
│                  │  └── INSERT outbox ─────────→│  outbox table    │
│                  │     COMMIT                   │  (id=UUIDv7)     │
└──────────────────┘                              └─────────┬────────┘
                                                            │ poll
                                                            │ FOR UPDATE
                                                            │ SKIP LOCKED
                                                            ▼
┌──────────────────┐  publish_with_headers       ┌──────────────────┐
│  Outbox          │  Nats-Msg-Id = outbox.id    │ NATS JetStream   │
│  Dispatcher      │────────────────────────────→│ R3, dedup 5min   │
│  (Rust task)     │←──── PubAck ─────────────── │ EVENTS stream    │
└──────────────────┘  duplicate=false│true       └─────────┬────────┘
       │                                                   │ pull
       │ UPDATE published_at                               ▼
       ▼                                          ┌──────────────────┐
   outbox row                                     │  Pull Consumer   │
                                                  │  AckExplicit     │
                                                  │  MaxDeliver=5    │
                                                  └─────────┬────────┘
                                                            │
                                          ┌─────────────────┼──────────────────┐
                                          ▼                 ▼                  ▼
                                    [permanent err]   [transient err]      [success]
                                    AckKind::Term     AckKind::Nak(5s)   double_ack
                                          │                 │                  │
                                          ▼                 ▼                  ▼
                                    DLQ stream         Retry với        BEGIN tx
                                                       BackOff:        ON CONFLICT
                                                       [1s,5s,30s,5m]  inbox table +
                                                                       business write
                                                                       COMMIT → ack
```

---

## 7. Khuyến nghị kết luận và roadmap cho thư viện của bạn

JetStream cung cấp một **substrate đủ mạnh** nhưng **chưa đủ** cho exactly-once business-critical out-of-the-box. Ba lớp phải có trong thư viện Rust của bạn:

**Lớp 1 — Wire-level guarantees (JetStream native):** R3 minimum, `duplicate_window` 5-15 phút (default 2 phút quá ngắn cho production retries), file storage không NFS, `sync_interval` cân nhắc giảm xuống 10-30s nếu rate cho phép, pull consumers + AckExplicit + double_ack, finite MaxDeliver với BackOff array, subscribe `MAX_DELIVERIES` advisories cho DLQ.

**Lớp 2 — Application-level reliability (thư viện phải provide):** transactional outbox với sqlx (postgres + mysql adapters), inbox/idempotency store với 3 backends pluggable (NATS KV mặc định, PostgreSQL, Redis), DLQ subject convention + replay tooling, retry với full-jitter exponential backoff, circuit breaker cho publish path khi NATS unreachable, fallback local SQLite buffer cho fail-soft modes.

**Lớp 3 — Operational (DX và observability):** tracing crate + OpenTelemetry với W3C `traceparent` header propagation tự động, metrics expose qua `metrics` crate (hits dedup, redeliveries, ack latency, DLQ depth), testcontainers + toxiproxy fixtures cho consumers, property-based tests cho idempotent handlers, builder API kiểu Service Bus với type-safe `Event` trait + derive macro.

**Cảnh báo trí tuệ kỹ thuật:** đừng over-promise "exactly-once" trong README. Synadia chính họ tránh từ này — gọi là "effectively-once via dedup + double-ack." Document rõ failure modes (Jepsen findings), khuyến cáo R3 + outbox + inbox cho production, và provide migration path lên Kafka cho scenarios vượt threshold (>500K msg/s sustained, >7 ngày retention, multi-partition atomic transactions).

**Decision matrix cuối:**

| Use case | Stack đề xuất |
|---|---|
| Microservice events thuần internal, hours retention, <100K msg/s | JetStream R3 + outbox + inbox (NATS KV) |
| Financial/orders, không tolerate loss | JetStream R3 + outbox (Postgres) + inbox (Postgres) + circuit breaker + fail-loud |
| IoT edge → cloud | JetStream leaf nodes + edge SQLite buffer + mirror cloud |
| Analytics pipeline, weeks retention, >1M msg/s | NATS-Kafka bridge: NATS operational + Kafka analytics |
| Multi-system atomic txn across many partitions | Kafka transactions hoặc Pulsar |
| Long workflows + compensations (sagas) | Temporal + JetStream events (no Rust SDK production-grade hiện tại — explicit state machine) |

Thư viện của bạn lấp **một khoảng trống thực sự** trong hệ sinh thái Rust. Không có "Service Bus for Rust" wrapping JetStream với idempotency + outbox built-in tồn tại tới April 2026. Build nó với honesty về limits, comprehensive testing chaos (Antithesis nếu budget cho phép), và document failure modes minh bạch thay vì marketing exactly-once. Đây là cách product thực sự chiến thắng được niềm tin của teams làm payments, orders, audit logs.
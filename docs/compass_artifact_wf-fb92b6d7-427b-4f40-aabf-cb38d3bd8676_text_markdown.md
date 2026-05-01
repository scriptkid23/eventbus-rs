# Kiến trúc Dead Letter Queue cho NATS JetStream

**NATS JetStream không có DLQ tích hợp sẵn — DLQ trong NATS luôn là một pattern kiến trúc bạn phải tự lắp ráp từ các primitive.** Đây là khẳng định nhất quán từ Synadia, Derek Collison (tác giả NATS), và R.I. Pienaar (Choria). Khác với SQS có `RedrivePolicy` hay RabbitMQ có `dead-letter-exchange` cấu hình một dòng, NATS chọn cách "tách hoàn toàn consumer khỏi stream semantics" với lý do: cùng một message có thể được tiêu thụ bởi hàng trăm consumer, và việc một consumer fail không có nghĩa là message đó "chết" với toàn hệ thống. Hệ quả thiết kế: bạn xây DLQ bằng cách kết hợp `MaxDeliver` + `BackOff` + advisory subjects + một stream chuyên dụng. Báo cáo này tổng hợp các pattern kiến trúc, trade-off, best practice và pitfall ở cấp độ thiết kế — không đi vào code.

---

## 1. DLQ là gì và tại sao cần trong event-driven systems

**DLQ (Dead Letter Queue) là nơi lưu trữ chuyên dụng cho các message không thể xử lý thành công sau khi đã thử lại đủ số lần.** Trong các hệ event-driven, DLQ giải quyết ba vấn đề cốt lõi: (1) chặn poison message gây block toàn bộ partition/queue, (2) tách biệt failure handling khỏi happy path để không làm chậm throughput, và (3) cung cấp audit trail cho các message bị từ chối nhằm phục vụ debug và compliance.

**Khác biệt cốt lõi giữa các broker** nằm ở mức độ "tự động" của DLQ. AWS SQS có DLQ thực sự native: cấu hình `maxReceiveCount` và `deadLetterTargetArn`, broker tự chuyển message khi vượt ngưỡng. RabbitMQ có DLX (Dead Letter Exchange) từ phiên bản 2.8, kích hoạt qua `nack(requeue=false)`, TTL hết hạn, hoặc queue đầy — message tự động được republish vào exchange chỉ định kèm header `x-death`. Kafka không có DLQ native (theo triết lý "dumb broker, smart consumer"); pattern phổ biến là DLT (Dead Letter Topic) qua Spring Kafka `DefaultErrorHandler` + `DeadLetterPublishingRecoverer`. **NATS JetStream nằm ở cực "DIY" nhất**: server chỉ phát ra advisory event khi `MaxDeliver` bị vượt, còn việc chuyển message vào đâu hoàn toàn do bạn tự thiết kế.

NATS JetStream cung cấp các primitive sau làm nền tảng cho mọi pattern DLQ: **`MaxDeliver`** (giới hạn số lần redelivery), **`AckWait`** (timeout đợi ack), **`BackOff`** (mảng delay theo từng lần thử lại, ví dụ `[5s, 30s, 5m, 30m, 2h]`), **`AckPolicy=explicit`** kèm bốn loại ack: `Ack` (thành công), `Nak`/`NakWithDelay` (retry ngay/có delay), `AckProgress` (gia hạn AckWait cho long-running job), và quan trọng nhất `AckTerm` (hủy redelivery vĩnh viễn — tín hiệu "vào DLQ ngay" của ứng dụng). Khi `MaxDeliver` bị vượt, message **không bị xóa khỏi source stream** mà chỉ đơn giản bị consumer đó "bỏ qua" — đây là điểm khác biệt quan trọng so với SQS/RabbitMQ.

---

## 2. Cơ chế native — Advisory subjects là chìa khóa

Hai advisory subject quyết định toàn bộ kiến trúc DLQ trong NATS JetStream:

- **`$JS.EVENT.ADVISORY.CONSUMER.MAX_DELIVERIES.<STREAM>.<CONSUMER>`** — phát khi message vượt `MaxDeliver`. Schema `io.nats.jetstream.advisory.v1.max_deliver`, payload chứa `stream`, `consumer`, `stream_seq`, `deliveries`, `timestamp`.
- **`$JS.EVENT.ADVISORY.CONSUMER.MSG_TERMINATED.<STREAM>.<CONSUMER>`** — phát khi consumer chủ động gọi `Term()`. Có thêm trường `reason` để mô tả nguyên nhân.

**Chú ý cực kỳ quan trọng về độ tin cậy của advisory**: advisory được publish theo cơ chế core NATS at-most-once. Nếu tại thời điểm phát không có subscriber nào đang lắng nghe, **server sẽ silently drop** advisory đó (xem hàm `publishAdvisory` trong `nats-server/server/jetstream_events.go`). Đây là gap lớn nhất khi dùng advisory làm trigger DLQ. **Cách khắc phục chuẩn: tạo một JetStream stream subscribe sẵn các advisory subject** — khi đã có stream consume, mỗi advisory phát ra sẽ được persist durably.

Một caveat thường bị bỏ sót: payload của advisory **chỉ chứa metadata, không bao giờ chứa message body**. Bạn luôn cần thực hiện một bước thứ hai — gọi `$JS.API.STREAM.MSG.GET.<stream>` với `stream_seq` — để lấy payload gốc. Điều này tạo ra failure mode nghiêm trọng nếu source stream dùng `WorkQueuePolicy` hoặc `InterestPolicy`: tới lúc DLQ processor đọc advisory thì message gốc có thể đã bị xóa. Vì vậy chỉ pattern advisory-driven mới phù hợp với source stream `LimitsPolicy`.

**Hạn chế của cách tiếp cận native** so với SQS/RabbitMQ: (1) không có trường config kiểu `dead_letter_subject` ở consumer level, mọi thứ phải tự đấu nối; (2) `MaxDeliver=-1` (mặc định) không phát advisory — phải set giá trị hữu hạn; (3) giảm `MaxDeliver` xuống không retroactively trigger DLQ cho message đã vượt ngưỡng cũ (issue nats-server #7148); (4) trên pull consumer, nếu không có client nào đang fetch, `AckWait` timeout không tự đẩy message tới trạng thái "max delivered" (discussion #4994) — phá vỡ kiến trúc scale-to-zero; (5) `BackOff` chỉ áp dụng cho AckWait timeout, còn `Nak()` thẳng thì redeliver ngay lập tức — phải dùng `NakWithDelay()` để honor backoff.

---

## 3. Năm pattern kiến trúc DLQ và trade-off

### 3.1 Pattern A — Separate DLQ Stream (advisory-driven, được Synadia khuyến nghị mặc định)

Một stream chuyên biệt `DLQ_<source>` được tạo với `subjects` filter là `$JS.EVENT.ADVISORY.CONSUMER.MAX_DELIVERIES.<SourceStream>.*` và `MSG_TERMINATED.<SourceStream>.*`. Một consumer "DLQ processor" subscribe vào DLQ stream này: với mỗi advisory, parse JSON, extract `stream_seq`, gọi `STREAM.MSG.GET` để lấy payload gốc, sau đó quyết định hành động — log, alert, archive, hoặc copy sang parking stream.

**Ưu điểm**: cô lập rõ ràng giữa happy-path traffic và failure traffic; có retention/replication policy độc lập; dễ alert trên độ sâu stream. **Nhược điểm**: topology hai-stream phức tạp hơn để vận hành; pattern này **vỡ nếu source stream dùng WorkQueue/Interest** vì payload có thể đã bị xóa khi DLQ processor lookup; advisory không durable nếu DLQ stream tạm thời unavailable. **Khi nào dùng**: làm pattern mặc định cho source stream `LimitsPolicy`, đặc biệt cho event log hoặc audit trail.

### 3.2 Pattern B — Subject-based DLQ (một stream nhiều subject DLQ)

Một stream duy nhất bind wildcard như `dlq.>` hoặc `*.dlq.>`, nhiều service publish thẳng vào theo convention naming: `<domain>.dlq.<event>` (ví dụ `orders.dlq.invalid_payload`), `dlq.<service>.<event>`, hoặc `<domain>.<event>.dlq`. Worker tự đếm `meta.NumDelivered`, khi đạt ngưỡng thì publish full payload + headers vào subject DLQ rồi ack message gốc.

**Ưu điểm**: chỉ một stream để vận hành; wildcard subscription cho phép một service DLQ-watcher duy nhất; dễ onboard service mới (chỉ cần publish theo convention). **Nhược điểm**: noisy neighbor — tất cả team chia sẻ retention/quota; ACL phức tạp với wildcard; khó áp dụng retention khác nhau theo service. **Khi nào dùng**: tổ chức nhỏ-vừa với strict naming discipline; platform team quản lý hạ tầng dùng chung.

### 3.3 Pattern C — Per-consumer DLQ vs Global DLQ

Đây là **trục thiết kế chéo** với Pattern A/B chứ không phải pattern riêng. Per-consumer DLQ tạo một DLQ stream cho mỗi worker (ví dụ `DLQ_orders_billing_worker`, `DLQ_orders_notification_worker`) để giải quyết "multi-consumer replay problem": khi một stream phục vụ N consumer và chỉ một consumer fail trên `seq=42`, replay vào subject gốc sẽ deliver lại tới TẤT CẢ consumer — kể cả những consumer đã xử lý thành công, gây side-effect nhân đôi (gửi email lần 2, charge thẻ lần 2). Global DLQ ngược lại tập trung tất cả vào một stream duy nhất.

| Tiêu chí | Per-consumer DLQ | Global DLQ |
|---|---|---|
| Isolation | Cao — failure giữa các worker tách biệt | Thấp — chia sẻ blast radius |
| An toàn replay | Cao — chỉ replay tới consumer mục tiêu | Cần replay subject riêng + headers |
| Multi-tenancy | Phù hợp tự nhiên với account-per-tenant | Phải có cross-account import/export |
| Chi phí storage | Cao hơn (nhiều stream nhỏ) | Thấp hơn |
| Phù hợp với | Workflow tài chính/billing đa người dùng | Internal tool, low-volume failures |

### 3.4 Pattern D — Retry queue + DLQ với exponential backoff

Đây là EIP "Delayed/Retry Channel" pattern. Trong NATS có ba style:

**D1 (baseline)**: dùng `BackOff` array trên consumer, ví dụ `[1s, 5s, 30s, 5m, 30m]` với `MaxDeliver=5`. Server giữ state retry, đơn giản nhất, không cần stream phụ. **D2 (Kafka-style tiered streams)**: chuỗi stream `RETRY_5S → RETRY_30S → RETRY_5M → DLQ_ORDERS`, mỗi stream có AckWait riêng theo tier. Pattern "phased backoff" của Spring Kafka tránh topic explosion: chia theo phase (3 phút retry mỗi 5s, 30 phút retry mỗi 45s, 6 giờ retry mỗi 2 phút) thay vì một stream mỗi delay. **D3 (NATS 2.12+ delayed delivery — emerging best practice)**: dùng header `Nats-Schedule-Time: @at <timestamp>` để worker republish message với delay server-side, không cần consumer state cho NakWithDelay. Sau N attempt thì publish thẳng vào DLQ subject.

**Khi nào dùng**: tích hợp với external API có rate limit hoặc dependency hay flaky. Pattern này **làm tăng latency end-to-end** nhưng bảo vệ downstream khỏi thundering herd. NATS 2.12+ là production-ready cho D3.

### 3.5 Pattern E — Parking lot (kho chứa dài hạn)

**DLQ và Parking Lot không phải là một**: DLQ chứa message vừa fail, Parking Lot chứa message đã được xác nhận (bởi automated quarantine hoặc human) là không thể auto-recover. Đây là pipeline DLQ hai tầng: source → DLQ → (retry processor đọc, đếm attempt qua header, nếu vượt threshold) → Parking Lot → human review.

Cấu hình stream: `Storage=file`, `Replicas=3`, `Retention=Limits`, `MaxAge` rất dài (90 ngày đến vô hạn), `DenyDelete=true` để bảo toàn audit trail. Truy cập hạn chế chỉ team SRE/platform. Tích hợp với ticketing (Jira/PagerDuty) hoặc BI tooling. **Phù hợp với**: ngành regulated (tài chính, healthcare), workflow có yêu cầu eventual recovery, tổ chức SRE trưởng thành. **Rủi ro**: nếu không có human process thực sự, parking lot trở thành "graveyard" — chỉ là silent data loss có vỏ bọc.

---

## 4. Các yếu tố thiết kế then chốt

### 4.1 Metadata enrichment — schema headers chuẩn cho DLQ

Khi worker ghi message vào DLQ stream (Pattern A Route 2 hoặc Pattern B), schema headers tối thiểu nên bao gồm: `X-Original-Subject`, `X-Original-Stream`, `X-Original-Seq` (con trỏ tới message gốc), `X-Original-Msg-Id` (carry-through `Nats-Msg-Id` để dedup khi replay), `X-Failure-Reason` (enum như `invalid_json`, `validation_failed`, `db_timeout`, `auth_failed`, `max_retries_exceeded`), `X-Failure-Class` (`transient`/`permanent`/`poison`), `X-Retry-Count` (lấy từ `meta.NumDelivered`), `X-Consumer` (consumer nào fail — cực quan trọng trong multi-consumer setup), `X-Service` + `X-Service-Version`, `X-Failed-At` (ISO-8601), `X-Correlation-Id` + W3C `traceparent` để link OpenTelemetry trace, `X-Tenant` cho multi-tenant routing, và `X-Replay-Count` để tránh infinite replay loop. Body giữ nguyên payload gốc (verbatim) hoặc bọc trong envelope JSON với field `data` nếu cần thêm `error_stack`.

### 4.2 Cấu hình stream cho DLQ stream

**`Storage=file`** (failures phải sống sót restart), **`Retention=LimitsPolicy`** (tuyệt đối không dùng WorkQueue/Interest cho DLQ — operator cần browse), **`Replicas=3`** match SLA primary, **`MaxAge`** từ 7 đến 30 ngày (default cộng đồng là 30 ngày, dài hơn source stream), **`MaxBytes`** có giới hạn cứng + **`Discard=old`** (an toàn hơn `Discard=new` — drop new failure tệ hơn evict cái cũ trong storm), **`AllowDirect=true`** (tăng tốc operator lookup), **`DenyDelete=true`** (audit integrity), **`DenyPurge=false`** (cho phép drain sau replay), **`DuplicateWindow`** 2-5 phút (cho phép idempotent re-publish). Cân nhắc bật `Republish` để mirror DLQ entries sang stream analytics riêng.

### 4.3 Cấu hình consumer cho worker sinh ra DLQ

`AckPolicy=Explicit` luôn luôn cho production. `AckWait` đặt ở 2-3× P99 processing time để tránh spurious redelivery. `MaxDeliver` từ 3-10 (không bao giờ là `-1` ở production); nếu dùng app-counted DLQ thì set `MaxDeliver` cao hơn ngưỡng code làm safety net (khuyến nghị Todd Beets/Synadia). `BackOff` dạng geometric: `[1s, 5s, 30s, 5m, 30m]` — tránh linear vì gây thundering herd. `MaxAckPending` bound theo concurrency × processing time để flow control. Pull consumer ưu tiên hơn push trong NATS hiện đại.

### 4.4 Idempotency và deduplication

Đặt **`Nats-Msg-Id` trên publish vào DLQ** với giá trị deterministic như `<original_stream>:<stream_seq>:<consumer>:<attempt>` để tránh double-write nếu worker crash giữa "publish DLQ" và "ack source". DLQ stream `DuplicateWindow` ≥ longest retry loop expected. **Nguyên tắc bất khả xâm phạm**: idempotent consumer là điều kiện cần (không phải tùy chọn) để replay an toàn — store một processed-message log keyed theo `stream_seq` hoặc business ID. Đây là điều khiến cả replay và at-least-once delivery trở nên an toàn bất kể DLQ scheme.

### 4.5 Phân loại failure — transient vs permanent

Nguyên tắc Conduktor: **transient errors heal, poison pills never do**. Transient (network timeout, DB connection, downstream 5xx, throttling) → retry với backoff, **không** vào DLQ ngay. Permanent / poison-pill (deserialization error, schema mismatch, business rule violation, validation, NullPointerException) → vào DLQ ngay từ lần đầu phát hiện qua `Term()` trong NATS hoặc đăng ký non-retryable trong Spring Kafka. Pattern Uber: network errors → chuỗi retry topic; null pointer/code error → thẳng vào DLT.

### 4.6 Observability

Metrics quan trọng phải dashboard và alert: **DLQ depth** (số message hiện tại), **DLQ ingestion rate** (msg/sec — phát hiện poison-pill flood), **age of oldest DLQ message** (phát hiện forgotten message), **growth rate** (slow leak), **retry distribution / `NumDelivered` histogram**, **consumer ack-pending count**. Trong NATS dùng `jetstream_stream_total_messages{stream="DLQ_*"}`, `jetstream_consumer_num_redelivered`, `jetstream_consumer_num_ack_pending` qua NATS Surveyor + Prometheus + Grafana dashboard #14725. Tiered alerting: info khi message đầu tiên xuất hiện (Slack), warn khi depth > N hoặc rate > 2× baseline (ticket), page khi age vượt SLA hoặc rate > 10× baseline.

---

## 5. Xử lý và phục hồi DLQ

**Manual inspection** trong NATS: `nats stream view DLQ_ORDERS` để liệt kê, `nats stream get ORDERS <seq>` để fetch payload gốc theo `stream_seq` từ advisory, sau đó quyết định discard/fix-and-replay/hot-fix-and-replay. **Automated reprocessing** có ba cách phổ biến: (1) timer-based replay drain DLQ với velocity controlled (giống SQS `StartMessageMoveTask`); (2) conditional replay — worker đọc DLQ, evaluate predicate (header `error_class=Transient`, age < 7d, retry count < N) rồi republish, ngược lại dead-store; (3) two-stage DLQ với DLQ triage → redrive queue (đã fix) → quarantine (broken vĩnh viễn).

**Replay back to original stream** là phần nguy hiểm nhất. **Multi-consumer hazard**: nếu source stream phục vụ nhiều consumer, republish vào subject gốc sẽ deliver tới mọi consumer. Mitigation: dùng dedicated replay subject (`orders.created.replay.billing`) chỉ consumer fail filter; hoặc set headers `X-Replay: true`, `X-Replay-Count`, `X-Original-Seq`, `X-Target-Consumer` rồi consumer check trước khi process. **Replay loop pitfall**: replay khi chưa fix root cause sẽ refill DLQ — bắt buộc cap số lần replay qua `X-Replay-Count`, yêu cầu deploy fix làm gating step trước mass replay.

**TTL và cleanup**: nguyên tắc DLQ TTL ≥ source TTL × 2. Tiered retention: DLQ nóng (recent, fast storage) + cold archive (S3) cho > 30 ngày. Trong Kafka có thể dùng compaction `cleanup.policy=compact` keyed theo business ID — chỉ giữ failure mới nhất per entity, tránh unbounded growth.

**Tooling ecosystem cho NATS DLQ**: `nats` CLI cho hầu hết tác vụ (`stream view`, `stream get`, `consumer info`, `events --js-advisory`); **NATS NUI** (open-source desktop UI); **StreamTrace** (commercial Docker UI, có batch replay với drain rate); **Synadia Console / Insights** với 100+ audit checks; **NATS Surveyor + Prometheus exporter** cho metrics. Hiện tại NATS chưa có nút "Replay" tích hợp — viết bash script lúc 3 giờ sáng vẫn là pain point chuẩn.

---

## 6. Best practices được tổng hợp

**Khi nào KHÔNG nên dùng DLQ**: (a) failure mode là downstream outage tác động TẤT CẢ message — để chúng ở source và retry tới khi recovery tốt hơn flooding DLQ; (b) yêu cầu strict global ordering — DLQ phá vỡ thứ tự; (c) hệ thống cần wait vô thời hạn cho dependency; (d) **không có review/recovery process** — DLQ không có process còn tệ hơn drop message. AWS cảnh báo rõ điểm này với FIFO use case như video EDL.

**Circuit breaker integration** (pattern AMQP "Kill Switch" của Olivier Mallassi): khi DLQ ingestion rate spike, *open* consumer-side breaker — pause consumer hoàn toàn thay vì drain source vào DLQ. Hai layered breaker: (1) external dependency breaker, (2) breaker disconnect consumer khỏi broker khi (1) open. Tránh "retry storm" tự DDoS dependency đang recover.

**Capacity planning**: size DLQ ~1-5% throughput của source làm upper bound; oversize trong giai đoạn early production. NATS dùng file storage với `max-bytes` và `max-age` có giới hạn; cân nhắc `discard=new` cho work queue (fail-fast) vs `discard=old` cho DLQ (preserve audit). Plan cho "incident burst" — schema break upstream có thể 10× normal volume trong vài phút.

**Define SLO cho DLQ**: max 1% message vào DLQ/ngày; oldest DLQ message age < 24h; 100% DLQ message review trong 48h. **Treat DLQ depth như SLI availability của data pipeline**, không phải metric phụ.

**Alerting thresholds**: depth > 0 (warn), depth > 100 (page), age oldest > 1h (warn), > 24h (page) hoặc > 50% retention period; rate > 10× baseline trong 5 phút (page). Trong SQS phải dùng `ApproximateNumberOfMessagesVisible` (không phải `NumberOfMessagesSent` — metric đó không count auto-moved DLQ message).

---

## 7. Pitfalls thường gặp và cách phòng tránh

**Infinite redelivery loop**: bắt buộc set `MaxDeliver` hữu hạn — case 2019 payment processor để loop 10,000 lần trong 6 giờ vì miss config này. **DLQ "black hole" / "car alarm in a parking lot"**: message tích lũy nhưng không ai xem — alert ngay khi có message đầu tiên, weekly review SLA, auto-create JIRA ticket per entry, mandatory drain-to-zero trước release.

**Memory/storage explosion**: schema break upstream có thể flood DLQ — bound DLQ size, circuit breaker pause consumer khi ingestion rate spike, pre-DLQ schema validation gate. **Lost messages during DLQ writes (atomicity)**: nếu DLQ publish thành công nhưng ack source fail (hoặc ngược lại) sẽ duplicate hoặc lost. Trong NATS publish vào DLQ stream trước, đợi pub-ack, *rồi* ack original — chấp nhận duplicate và rely vào idempotency.

**Broken ordering**: DLQ và ordering largely incompatible — nếu order là tối quan trọng thì *halt consumer* (kill switch) thay vì dead-letter. **Schema evolution** cho old DLQ messages: lưu schema ID/version trong header; dùng Schema Registry với `BACKWARD`/`FULL` compatibility; có migration script cho old payload.

**NATS-specific pitfalls đặc biệt**: (1) WorkQueue + advisory race — message bị xóa trước khi DLQ processor đọc advisory → dùng LimitsPolicy upstream + Mirror, hoặc worker-side DLQ republish trước ack; (2) AckWait timeout không trigger MaxDeliver nếu không có client fetching → giữ ít nhất một warm consumer; (3) Multi-consumer replay duplication — đã giải thích ở §5; (4) `Nats-Msg-Id` dedup window collision khi replay trong 2 phút (hiếm vì DLQ message thường hours old); (5) `BackOff` chỉ áp dụng cho AckWait timeout không cho `Nak()` — phải dùng `NakWithDelay()`; (6) Issue nats-server #7817 (cuối 2025) báo cáo message loss với WorkQueue+R3+max-deliver — ưu tiên LimitsPolicy upstream.

---

## 8. Reference architecture cho event bus production

### 8.1 Topology cốt lõi end-to-end

Producers → **Main Stream** (`ORDERS`, retention=limits, R=3, file storage) → các Consumers (`MaxDeliver=5`, `BackOff=[1s,5s,1m,5m,10m]`, `AckExplicit`, `AckWait=30s`) → khi exhausted, server emit advisory → **DLQ Stream** (`DLQ_ORDERS`, subjects `$JS.EVENT.ADVISORY.CONSUMER.MAX_DELIVERIES.ORDERS.*` và `MSG_TERMINATED.ORDERS.*`, R=3, max-age 7-30 ngày) → song song hai consumer: **DLQ Inspector UI** (read-only triage qua StreamTrace/NUI/Synadia Console) và **Replay Service** (parse advisory → `GetMsg(orig_seq)` → quyết định drop/repair/replay → publish vào dedicated replay subject với enriched headers, hoặc archive sang S3/MinIO cho cold storage).

Một best practice khác: **tạo một archive stream `$JS.EVENT.>` luôn có sẵn** với retention vài giờ, bất kể có DLQ hay không (recommendation R.I. Pienaar). Đây là baseline ops practice để debug bất kỳ JetStream incident nào.

### 8.2 Multi-tenant SaaS — account-per-tenant là pattern chuẩn

Có ba model multi-tenancy: **(A) per-tenant DLQ stream trong per-tenant account** (canonical SaaS pattern — strongest isolation, GDPR-friendly, mỗi tenant có quota JetStream riêng); **(B) shared account, per-tenant DLQ stream tagged by subject** (đơn giản hơn, nhưng ACL phức tạp, không có quota per-tenant ở mức JetStream); **(C) shared global DLQ với tenant-tagging header** (worst isolation, data leak risk, không GDPR-safe). Khuyến nghị production: Model A cho SaaS, Model B chỉ cho internal multi-team microservices, Model C chỉ cho non-sensitive ops telemetry.

Cross-tenant ops querying: dùng **aggregator account** với `imports` của mỗi tenant DLQ dưới dạng *mirror* (read-only, không propagate delete — perfect cho ops); hoặc `$SYS` observers cho signals (không cho payload inspection). Authentication: decentralized JWT (operator → account → user keys qua `nsc`) — đây là path Synadia Cloud và hầu hết production SaaS dùng. Pin DLQ access cho user `dlq-ops` chuyên biệt với subscribe-only permissions.

### 8.3 Microservices và saga integration

**Service-owned DLQ (recommended)**: mỗi team microservice own `<service>.events.>` và `<service>.dlq.>`, chịu trách nhiệm triage. Manage qua NACK CRDs (`Stream`, `Consumer`) trong GitOps với Flux/Argo — DLQ topology là code. Convention `buildDlqSubject(serviceName, originalPattern)` tránh naming drift.

**Saga choreography**: mỗi saga step failure → main stream → MaxDeliver → DLQ. Compensation logic **không** nên ở DLQ processor; thay vào đó DLQ trigger operational alert và optionally auto-publish compensation event sau khi human approve replay. **Saga orchestration** (Vitrifi pattern với engine "Shar"): orchestrator persist workflow state trong JetStream KV, subscribe DLQ advisories và quyết định retry-with-compensation vs hard fail. **Outbox pattern** (`cms103/jetstream-outbox`) là complementary với DLQ — outbox solve publish-side với `Nats-Msg-Id = outbox row id` cho native dedup; DLQ solve consumer-side. Pair với **inbox table** keyed `Nats-Msg-Id` để exactly-once processing — đây mới là cái loại bỏ multi-consumer replay duplication.

### 8.4 Edge và geo-distribution

**Leaf nodes**: edge site có local DLQ stream (store-and-forward khi disconnect) → hub có aggregator stream `--source LEAF_DLQ --js-domain leaf`. Hub-to-leaf commands (như "replay these IDs") đi qua mirror stream trên leaf để command survive uplink outage. **Super-cluster**: stream replication **không** cross gateway boundaries — stream tồn tại trong đúng một cluster. Cross-region DLQ: per-region DLQ + global aggregator mirror/source mỗi regional DLQ; mirror preserve sequence number (forensic ordering tốt) còn source thì interleave. **JetStream domains** trên mỗi leaf để prevent stream collision và resource bleed giữa hub và leaf.

---

## Kết luận

DLQ trong NATS JetStream là **bài toán kiến trúc, không phải bài toán cấu hình**. Đây là điểm khác biệt mindset cốt lõi mà team đến từ SQS/RabbitMQ/Kafka cần thấm: bạn đang lắp ghép một workflow từ advisory subjects, dedicated stream, idempotent consumer và replay tooling — chứ không phải bật một cờ trên consumer config. Lựa chọn pattern kiến trúc nên drive bởi retention policy của source stream (LimitsPolicy → Pattern A advisory-driven; WorkQueue/Interest → Pattern B worker-driven republish), bởi mức độ multi-consumer (nhiều consumer → per-consumer DLQ), và bởi nature của failure (transient-heavy → Pattern D với NATS 2.12+ scheduled delivery; compliance-heavy → Pattern E parking lot).

**Ba nguyên tắc không thể vi phạm trong production**: (1) idempotent consumer là điều kiện cần — không phải optional, vì nó làm replay và at-least-once delivery an toàn bất kể DLQ scheme nào; (2) phải có review/recovery process trước khi deploy DLQ — nếu không, DLQ chỉ là silent data graveyard và còn tệ hơn drop message; (3) advisory durability không được tự nhiên đảm bảo — phải có stream subscribe sẵn advisory subject từ trước để không mất tín hiệu failure.

Insight quan trọng nhất: triết lý "DIY DLQ" của NATS không phải là khuyết điểm mà là quyết định thiết kế có chủ đích — nó cho phép cùng một message được tiêu thụ bởi nhiều consumer độc lập, mỗi consumer có policy retry và DLQ riêng, mà không có broker nào áp đặt semantic "dead" toàn cục. Điều này phù hợp với bản chất pub/sub của event bus đa-consumer hơn nhiều so với mô hình queue 1-1 truyền thống. Đánh đổi là bạn phải build tooling (replay UI, dashboard, alert) — và đây chính là pain point lớn nhất hiện tại của hệ sinh thái, được lấp dần bởi StreamTrace, NUI, và Synadia Console nhưng vẫn chưa có "nút Replay" first-class trong core NATS.
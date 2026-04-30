use opentelemetry::{
    global,
    metrics::{Counter, Histogram},
    KeyValue,
};

/// Holds all OTel metric instruments for eventbus-rs.
/// Create once and share via `Arc`. Instruments record against the global meter provider.
pub struct BusMetrics {
    pub publish_total:       Counter<u64>,
    pub publish_duration_ms: Histogram<f64>,
    pub consume_total:       Counter<u64>,
    pub consume_duration_ms: Histogram<f64>,
    pub redeliveries_total:  Counter<u64>,
    pub dlq_total:           Counter<u64>,
    pub outbox_dispatch_ms:  Histogram<f64>,
    pub idempotency_hits:    Counter<u64>,
}

impl BusMetrics {
    /// Initialize all instruments against the global meter provider.
    pub fn new() -> Self {
        let meter = global::meter("eventbus-rs");

        Self {
            publish_total: meter
                .u64_counter("eventbus.publish.total")
                .with_description("Total publish attempts")
                .init(),

            publish_duration_ms: meter
                .f64_histogram("eventbus.publish.duration_ms")
                .with_description("Publish latency in milliseconds")
                .init(),

            consume_total: meter
                .u64_counter("eventbus.consume.total")
                .with_description("Total messages consumed")
                .init(),

            consume_duration_ms: meter
                .f64_histogram("eventbus.consume.duration_ms")
                .with_description("Handler execution latency in milliseconds")
                .init(),

            redeliveries_total: meter
                .u64_counter("eventbus.redeliveries.total")
                .with_description("Total message redeliveries")
                .init(),

            dlq_total: meter
                .u64_counter("eventbus.dlq.total")
                .with_description("Total messages sent to DLQ")
                .init(),

            outbox_dispatch_ms: meter
                .f64_histogram("eventbus.outbox.dispatch_ms")
                .with_description("Outbox dispatch latency in milliseconds")
                .init(),

            idempotency_hits: meter
                .u64_counter("eventbus.idempotency.hits")
                .with_description("Duplicate messages detected by idempotency store")
                .init(),
        }
    }

    /// Record a successful or duplicate publish
    pub fn record_publish(&self, subject: &str, stream: &str, duplicate: bool, duration_ms: f64) {
        let attrs = [
            KeyValue::new("event.subject", subject.to_string()),
            KeyValue::new("nats.stream", stream.to_string()),
            KeyValue::new("duplicate", duplicate.to_string()),
        ];
        self.publish_total.add(1, &attrs);
        self.publish_duration_ms.record(duration_ms, &attrs[..2]);
    }

    /// Record a consume result: "success", "transient", or "permanent"
    pub fn record_consume(&self, subject: &str, consumer: &str, result: &str, duration_ms: f64) {
        let attrs = [
            KeyValue::new("event.subject", subject.to_string()),
            KeyValue::new("consumer", consumer.to_string()),
            KeyValue::new("result", result.to_string()),
        ];
        self.consume_total.add(1, &attrs);
        self.consume_duration_ms.record(duration_ms, &attrs[..2]);
    }

    /// Record an idempotency hit (duplicate detected)
    pub fn record_idempotency_hit(&self, backend: &str) {
        self.idempotency_hits
            .add(1, &[KeyValue::new("backend", backend.to_string())]);
    }
}

impl Default for BusMetrics {
    fn default() -> Self {
        Self::new()
    }
}

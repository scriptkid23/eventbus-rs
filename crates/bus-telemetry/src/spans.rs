use opentelemetry::{
    global::{self, BoxedSpan},
    trace::Tracer,
    Context,
};

const TRACER_NAME: &str = "eventbus-rs";

/// Create a span for a publish operation.
pub fn publish_span(_subject: &str, _msg_id: &str, _stream: &str) -> BoxedSpan {
    let tracer = global::tracer(TRACER_NAME);
    tracer.start_with_context("eventbus.publish", &Context::current())
}

/// Create a span for a consumer receive operation.
pub fn receive_span(
    _subject: &str,
    _stream_seq: u64,
    _delivered: u64,
    parent: Context,
) -> BoxedSpan {
    let tracer = global::tracer(TRACER_NAME);
    tracer.start_with_context("eventbus.receive", &parent)
}

/// Create a span for handler execution.
pub fn handle_span(_msg_id: &str, _handler_name: &str, parent: Context) -> BoxedSpan {
    let tracer = global::tracer(TRACER_NAME);
    tracer.start_with_context("eventbus.handle", &parent)
}

/// Create a span for outbox dispatch.
pub fn outbox_dispatch_span(_outbox_id: &str, _attempts: i32) -> BoxedSpan {
    let tracer = global::tracer(TRACER_NAME);
    tracer.start_with_context("eventbus.outbox.dispatch", &Context::current())
}

/// Create a span for idempotency check.
pub fn idempotency_span(_msg_id: &str, _backend: &str) -> BoxedSpan {
    let tracer = global::tracer(TRACER_NAME);
    tracer.start_with_context("eventbus.idempotency.check", &Context::current())
}

/// Marker re-export for symmetry with `BusMetrics`. Reserved for a future builder API.
pub struct SpanBuilder;

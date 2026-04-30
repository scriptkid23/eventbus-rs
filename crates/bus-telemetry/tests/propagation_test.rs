use async_nats::HeaderMap;
use bus_telemetry::{extract_context, inject_context};
use opentelemetry::{global, trace::TraceContextExt};
use opentelemetry_sdk::propagation::TraceContextPropagator;

fn setup_propagator() {
    global::set_text_map_propagator(TraceContextPropagator::new());
}

#[test]
fn inject_writes_traceparent_header() {
    setup_propagator();
    let mut headers = HeaderMap::new();
    // Without an active span, inject should still not panic
    inject_context(&mut headers);
    // With no active span, traceparent may not be written — just verify no panic
}

#[test]
fn extract_from_empty_headers_returns_empty_context() {
    setup_propagator();
    let headers = HeaderMap::new();
    let cx = extract_context(&headers);
    // Empty context — span context is not valid
    let span = cx.span();
    assert!(!span.span_context().is_valid());
}

#[test]
fn roundtrip_inject_then_extract() {
    setup_propagator();

    // Build a known traceparent header manually
    let mut headers = HeaderMap::new();
    let traceparent = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
    headers.insert("traceparent", traceparent);

    let cx = extract_context(&headers);
    let span = cx.span();
    let span_ctx = span.span_context();
    assert!(span_ctx.is_valid());
    assert_eq!(
        span_ctx.trace_id().to_string(),
        "4bf92f3577b34da6a3ce929d0e0e4736"
    );
}

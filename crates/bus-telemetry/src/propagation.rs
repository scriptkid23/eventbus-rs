use async_nats::HeaderMap;
use opentelemetry::{global, Context};

/// NATS header map adapter for OTel TextMap inject
struct NatsHeaderInjector<'a>(&'a mut HeaderMap);

impl<'a> opentelemetry::propagation::Injector for NatsHeaderInjector<'a> {
    fn set(&mut self, key: &str, value: String) {
        self.0.insert(key, value.as_str());
    }
}

/// NATS header map adapter for OTel TextMap extract
struct NatsHeaderExtractor<'a>(&'a HeaderMap);

impl<'a> opentelemetry::propagation::Extractor for NatsHeaderExtractor<'a> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(|v| v.as_str())
    }

    fn keys(&self) -> Vec<&str> {
        // HeaderMap does not expose an iterator in all versions — return known keys
        vec!["traceparent", "tracestate"]
    }
}

/// Inject the current span context as W3C `traceparent` into NATS message headers.
/// Call this before publishing a message.
pub fn inject_context(headers: &mut HeaderMap) {
    let cx = Context::current();
    global::get_text_map_propagator(|propagator| {
        propagator.inject_context(&cx, &mut NatsHeaderInjector(headers));
    });
}

/// Extract the W3C `traceparent` from NATS message headers and return the context.
/// Call this when receiving a message to create a child span.
pub fn extract_context(headers: &HeaderMap) -> Context {
    global::get_text_map_propagator(|propagator| propagator.extract(&NatsHeaderExtractor(headers)))
}

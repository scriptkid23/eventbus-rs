use async_nats::HeaderMap;
use opentelemetry::Context;

pub fn inject_context(_headers: &mut HeaderMap) {}
pub fn extract_context(_headers: &HeaderMap) -> Context {
    Context::current()
}

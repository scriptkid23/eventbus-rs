use crate::id::MessageId;
use serde::{de::DeserializeOwned, Serialize};
use std::borrow::Cow;

/// Marker trait for all events published through the event bus.
///
/// Implement manually or use `#[derive(Event)]` from the `bus-macros` crate.
/// Every implementor must have a stable, unique `message_id()` for idempotency
/// and deduplication.
pub trait Event: Serialize + DeserializeOwned + Send + Sync + 'static {
    /// NATS subject for this event.
    fn subject(&self) -> Cow<'_, str>;

    /// Unique identifier used as the message ID header and idempotency key.
    fn message_id(&self) -> MessageId;

    /// Aggregate type used for outbox routing.
    fn aggregate_type() -> &'static str
    where
        Self: Sized,
    {
        "default"
    }
}


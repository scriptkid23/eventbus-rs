use crate::{error::BusError, event::Event};
use async_trait::async_trait;

/// Receipt returned after a successful publish.
#[derive(Debug, Clone)]
pub struct PubReceipt {
    /// Name of the stream that received the message.
    pub stream: String,

    /// Stream sequence number assigned to this message.
    pub sequence: u64,

    /// True when backend deduplication suppressed this publish.
    pub duplicate: bool,

    /// True when the message was stored locally instead of published remotely.
    pub buffered: bool,
}

/// Publishes events to the event bus.
///
/// The generic methods make this trait intentionally not object safe.
#[async_trait]
pub trait Publisher: Send + Sync {
    /// Publish a single event.
    async fn publish<E: Event>(&self, event: &E) -> Result<PubReceipt, BusError>;

    /// Publish multiple events sequentially by default.
    async fn publish_batch<E: Event>(&self, events: &[E]) -> Result<Vec<PubReceipt>, BusError> {
        let mut receipts = Vec::with_capacity(events.len());

        for event in events {
            receipts.push(self.publish(event).await?);
        }

        Ok(receipts)
    }
}


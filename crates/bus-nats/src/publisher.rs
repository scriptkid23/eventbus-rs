use crate::client::NatsClient;
use async_nats::HeaderMap;
use async_trait::async_trait;
use bus_core::{error::BusError, event::Event, publisher::{PubReceipt, Publisher}};
use bytes::Bytes;

/// NATS JetStream implementation of `Publisher`.
/// Attaches `Nats-Msg-Id` header for server-side deduplication.
#[derive(Clone)]
pub struct NatsPublisher {
    client: NatsClient,
}

impl NatsPublisher {
    pub fn new(client: NatsClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl Publisher for NatsPublisher {
    async fn publish<E: Event>(&self, event: &E) -> Result<PubReceipt, BusError> {
        let subject = event.subject().into_owned();
        let msg_id = event.message_id().to_string();
        let payload = serde_json::to_vec(event).map_err(BusError::Serde)?;

        let mut headers = HeaderMap::new();
        headers.insert("Nats-Msg-Id", msg_id.as_str());

        let ack_future = self
            .client
            .js
            .publish_with_headers(subject, headers, Bytes::from(payload))
            .await
            .map_err(|e| BusError::Publish(e.to_string()))?;

        let ack = ack_future
            .await
            .map_err(|e| BusError::Publish(e.to_string()))?;

        Ok(PubReceipt {
            stream:    ack.stream.to_string(),
            sequence:  ack.sequence,
            duplicate: ack.duplicate,
            buffered:  false,
        })
    }
}

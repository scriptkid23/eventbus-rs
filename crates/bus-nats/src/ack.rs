use async_nats::jetstream::{AckKind, Message};
use bus_core::error::BusError;
use std::time::Duration;

/// Send a double-ack (waits for server confirmation).
/// This eliminates redelivery due to lost ack — required for effectively-once consumption.
pub async fn double_ack(msg: &Message) -> Result<(), BusError> {
    msg.double_ack()
        .await
        .map_err(|e| BusError::Nats(e.to_string()))
}

/// NAK with a specific delay before redelivery.
pub async fn nak_with_delay(msg: &Message, delay: Duration) -> Result<(), BusError> {
    msg.ack_with(AckKind::Nak(Some(delay)))
        .await
        .map_err(|e| BusError::Nats(e.to_string()))
}

/// Terminate message — no further redelivery, triggers MSG_TERMINATED advisory.
pub async fn term(msg: &Message) -> Result<(), BusError> {
    msg.ack_with(AckKind::Term)
        .await
        .map_err(|e| BusError::Nats(e.to_string()))
}

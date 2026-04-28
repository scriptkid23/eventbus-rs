use thiserror::Error;

/// Errors returned by handler implementations.
#[derive(Debug, Error)]
pub enum HandlerError {
    /// Transient failure: the message should be retried.
    #[error("transient: {0}")]
    Transient(String),

    /// Permanent failure: the message should not be retried.
    #[error("permanent: {0}")]
    Permanent(String),
}

/// Top-level error type for event bus operations.
#[derive(Debug, Error)]
pub enum BusError {
    #[error("nats: {0}")]
    Nats(String),

    #[error("publish: {0}")]
    Publish(String),

    #[error("outbox: {0}")]
    Outbox(String),

    #[error("idempotency: {0}")]
    Idempotency(String),

    #[error("serialization: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("handler: {0}")]
    Handler(#[from] HandlerError),

    #[error("nats unavailable")]
    NatsUnavailable,
}


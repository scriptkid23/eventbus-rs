use crate::{error::HandlerError, event::Event, id::MessageId};
use async_trait::async_trait;
use tracing::Span;

/// Context passed to every handler invocation.
#[derive(Debug)]
pub struct HandlerCtx {
    /// The message ID associated with this delivery.
    pub msg_id: MessageId,

    /// Stream sequence number for this message.
    pub stream_seq: u64,

    /// Number of times this message has been delivered.
    pub delivered: u64,

    /// Subject this message was received on.
    pub subject: String,

    /// Active tracing span for attaching handler work.
    pub span: Span,
}

/// Processes a strongly typed event received from the event bus.
#[async_trait]
pub trait EventHandler<E: Event>: Send + Sync + 'static {
    async fn handle(&self, ctx: HandlerCtx, event: E) -> Result<(), HandlerError>;
}


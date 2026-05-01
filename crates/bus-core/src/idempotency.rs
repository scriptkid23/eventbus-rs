use crate::{error::BusError, id::MessageId};
use async_trait::async_trait;
use std::time::Duration;

/// Backend-agnostic idempotency store for deduplicating handler executions.
///
/// Implementations must use atomic operations to remain safe under concurrent
/// delivery.
#[async_trait]
pub trait IdempotencyStore: Send + Sync {
    /// Attempt to claim this message ID for processing.
    ///
    /// Returns `Ok(true)` when this is the first time the key has been seen.
    /// Returns `Ok(false)` when the key already exists.
    async fn try_insert(&self, key: &MessageId, ttl: Duration) -> Result<bool, BusError>;

    /// Mark a previously inserted key as successfully processed.
    async fn mark_done(&self, key: &MessageId) -> Result<(), BusError>;

    /// Release a claim for work that did not complete and should be retried.
    async fn release(&self, key: &MessageId) -> Result<(), BusError>;
}

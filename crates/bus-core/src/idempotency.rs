use crate::{error::BusError, id::MessageId};
use async_trait::async_trait;
use std::time::Duration;

/// Result of a `try_claim` call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimOutcome {
    /// First time the key has been seen — caller now owns the pending claim.
    Claimed,
    /// Key already exists in `pending` state; another worker may be holding
    /// the claim, or a previous attempt did not complete. Caller should
    /// proceed only if it can guarantee no concurrent execution (e.g. a
    /// JetStream redelivery to the same consumer).
    AlreadyPending,
    /// Key exists in `done` state; the message was successfully processed
    /// before. Caller MUST treat this as a duplicate and ack without running
    /// the handler.
    AlreadyDone,
}

/// Backend-agnostic idempotency store for deduplicating handler executions.
///
/// Implementations must use atomic operations to remain safe under concurrent
/// delivery.
#[async_trait]
pub trait IdempotencyStore: Send + Sync {
    /// State-aware claim. Atomically inserts the key in `pending` state if
    /// absent, otherwise reports the existing state.
    async fn try_claim(
        &self,
        key: &MessageId,
        ttl: Duration,
    ) -> Result<ClaimOutcome, BusError>;

    /// Mark a previously claimed key as successfully processed.
    async fn mark_done(&self, key: &MessageId) -> Result<(), BusError>;

    /// Release a `pending` claim so the message can be retried.
    async fn release(&self, key: &MessageId) -> Result<(), BusError>;
}

#[cfg(test)]
mod tests {
    use super::ClaimOutcome;

    #[test]
    fn claim_outcome_variants_exist() {
        let _ = ClaimOutcome::Claimed;
        let _ = ClaimOutcome::AlreadyPending;
        let _ = ClaimOutcome::AlreadyDone;
    }

    #[test]
    fn claim_outcome_is_eq_and_debug() {
        assert_eq!(ClaimOutcome::Claimed, ClaimOutcome::Claimed);
        assert_ne!(ClaimOutcome::Claimed, ClaimOutcome::AlreadyDone);
        let _ = format!("{:?}", ClaimOutcome::AlreadyPending);
    }
}

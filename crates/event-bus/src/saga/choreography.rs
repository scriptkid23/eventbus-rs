use async_trait::async_trait;
use bus_core::{event::Event, handler::HandlerCtx};

/// A single step in a choreography-based saga.
///
/// On success: emits `Output` event to drive the next step.
/// On failure: emits `Compensation` event to trigger rollback in upstream services.
///
/// Suitable for linear flows with ≤ 4 participants. No central state is stored.
#[async_trait]
pub trait ChoreographyStep<E: Event>: Send + Sync {
    /// The event emitted when this step succeeds
    type Output: Event;

    /// The event emitted when this step fails (compensation)
    type Compensation: Event;

    async fn execute(
        &self,
        ctx: &HandlerCtx,
        event: E,
    ) -> Result<Self::Output, Self::Compensation>;
}

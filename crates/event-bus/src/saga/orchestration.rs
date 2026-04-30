use bus_core::{event::Event, id::MessageId};
use serde::{de::DeserializeOwned, Serialize};
use std::borrow::Cow;

/// Object-safe view of an [`Event`] used so saga transitions can carry a
/// heterogeneous list of commands. `bus_core::Event` is not object-safe
/// (it requires `DeserializeOwned`), so the orchestrator type-erases via
/// this companion trait. Any `E: Event` implements `SagaEvent` via a
/// blanket impl.
pub trait SagaEvent: Send + Sync {
    /// NATS subject for the carried event.
    fn subject(&self) -> Cow<'_, str>;

    /// Stable message id for deduplication.
    fn message_id(&self) -> MessageId;

    /// Serialize the carried event as JSON, suitable for publishing as the
    /// payload of a NATS message.
    fn to_json(&self) -> Result<Vec<u8>, serde_json::Error>;
}

impl<E: Event> SagaEvent for E {
    fn subject(&self) -> Cow<'_, str> {
        Event::subject(self)
    }

    fn message_id(&self) -> MessageId {
        Event::message_id(self)
    }

    fn to_json(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }
}

/// Defines the state machine for an orchestrated saga.
///
/// The orchestrator stores durable state in `eventbus_sagas` (Postgres).
/// Each incoming event triggers `transition()` which returns the next state
/// and a set of commands (events) to emit.
pub trait SagaDefinition: Send + Sync + 'static {
    /// Serializable saga state — stored as JSONB in Postgres
    type State: Serialize + DeserializeOwned + Send + Sync;

    /// The event type that drives this saga forward
    type Event: Event;

    /// Globally unique ID for this saga instance
    fn saga_id(&self) -> &str;

    /// Pure function: given current state + incoming event, return what happens next.
    /// Must not have side effects — side effects happen via emitted events.
    fn transition(
        &self,
        state: &Self::State,
        event: &Self::Event,
    ) -> SagaTransition<Self::State>;
}

/// Result of a saga state machine transition
pub enum SagaTransition<S> {
    /// Move to `next_state` and emit these events (commands to downstream services)
    Advance {
        next_state: S,
        emit:       Vec<Box<dyn SagaEvent>>,
    },

    /// Something failed — emit compensation events and mark saga as compensating
    Compensate {
        emit: Vec<Box<dyn SagaEvent>>,
    },

    /// All steps completed successfully
    Complete,

    /// Unrecoverable failure
    Fail(String),
}

/// Handle to a running saga instance
pub struct SagaHandle {
    pub saga_id: String,
}

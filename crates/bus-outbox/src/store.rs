#[cfg(feature = "postgres-outbox")]
use async_trait::async_trait;
#[cfg(feature = "postgres-outbox")]
use bus_core::{error::BusError, event::Event, id::MessageId};
#[cfg(feature = "postgres-outbox")]
use uuid::Uuid;

/// A single pending row from the outbox table.
#[cfg(feature = "postgres-outbox")]
#[derive(Debug, Clone)]
pub struct OutboxRow {
    pub id:       Uuid,
    pub subject:  String,
    pub payload:  serde_json::Value,
    pub headers:  serde_json::Value,
    pub attempts: i32,
}

/// Backend-agnostic interface for storing and querying outbox entries.
#[cfg(feature = "postgres-outbox")]
#[async_trait]
pub trait OutboxStore: Send + Sync {
    /// Persist an event inside an existing database transaction.
    async fn insert<'tx, E: Event>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Postgres>,
        event: &E,
    ) -> Result<(), BusError>;

    /// Fetch up to `limit` unpublished rows ordered by `created_at`.
    async fn fetch_pending(&self, limit: u32) -> Result<Vec<OutboxRow>, BusError>;

    /// Record a successful publish for the given message ID.
    async fn mark_published(&self, id: &MessageId) -> Result<(), BusError>;

    /// Record a delivery failure and its error message.
    async fn mark_failed(&self, id: &MessageId, error: &str) -> Result<(), BusError>;
}

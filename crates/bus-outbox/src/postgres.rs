use crate::store::{OutboxRow, OutboxStore};
use async_trait::async_trait;
use bus_core::{error::BusError, event::Event, id::MessageId};
use sqlx::{PgPool, Postgres, Transaction};

/// PostgreSQL-backed outbox store.
#[derive(Clone)]
pub struct PostgresOutboxStore {
    pool: PgPool,
}

impl PostgresOutboxStore {
    /// Create a new store backed by the given connection pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl OutboxStore for PostgresOutboxStore {
    async fn insert<'tx, E: Event>(
        &self,
        _tx: &mut Transaction<'tx, Postgres>,
        _event: &E,
    ) -> Result<(), BusError> {
        todo!("implemented in Task 9")
    }

    async fn fetch_pending(&self, _limit: u32) -> Result<Vec<OutboxRow>, BusError> {
        todo!("implemented in Task 9")
    }

    async fn mark_published(&self, _id: &MessageId) -> Result<(), BusError> {
        todo!("implemented in Task 9")
    }

    async fn mark_failed(&self, _id: &MessageId, _error: &str) -> Result<(), BusError> {
        todo!("implemented in Task 9")
    }
}

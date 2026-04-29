use async_trait::async_trait;
use bus_core::{error::BusError, id::MessageId, idempotency::IdempotencyStore};
use sqlx::PgPool;
use std::time::Duration;

/// PostgreSQL-backed idempotency store that prevents duplicate handler executions.
#[derive(Clone)]
pub struct PostgresIdempotencyStore {
    pool:     PgPool,
    consumer: String,
}

impl PostgresIdempotencyStore {
    /// Create a new store for the given consumer name.
    pub fn new(pool: PgPool, consumer: String) -> Self {
        Self { pool, consumer }
    }
}

#[async_trait]
impl IdempotencyStore for PostgresIdempotencyStore {
    async fn try_insert(&self, _key: &MessageId, _ttl: Duration) -> Result<bool, BusError> {
        todo!("implemented in Task 10")
    }

    async fn mark_done(&self, _key: &MessageId) -> Result<(), BusError> {
        todo!("implemented in Task 10")
    }
}

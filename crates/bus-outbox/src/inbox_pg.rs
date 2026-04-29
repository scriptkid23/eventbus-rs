use async_trait::async_trait;
use bus_core::{error::BusError, id::MessageId, idempotency::IdempotencyStore};
use sqlx::PgPool;
use std::time::Duration;

/// PostgreSQL-backed idempotency store that prevents duplicate handler executions.
///
/// Uses `INSERT ... ON CONFLICT DO NOTHING` for atomic, race-free deduplication.
/// Each consumer name has an independent namespace via the `(consumer, message_id)`
/// composite primary key.
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
    async fn try_insert(&self, key: &MessageId, _ttl: Duration) -> Result<bool, BusError> {
        let result = sqlx::query(
            r#"INSERT INTO eventbus_inbox (message_id, consumer, status)
               VALUES ($1, $2, 'pending')
               ON CONFLICT (consumer, message_id) DO NOTHING"#,
        )
        .bind(key.to_string())
        .bind(&self.consumer)
        .execute(&self.pool)
        .await
        .map_err(|e| BusError::Idempotency(e.to_string()))?;

        // rows_affected == 1 means inserted (first time), 0 means conflict (duplicate)
        Ok(result.rows_affected() == 1)
    }

    async fn mark_done(&self, key: &MessageId) -> Result<(), BusError> {
        sqlx::query(
            "UPDATE eventbus_inbox SET status = 'done' WHERE consumer = $1 AND message_id = $2",
        )
        .bind(&self.consumer)
        .bind(key.to_string())
        .execute(&self.pool)
        .await
        .map_err(|e| BusError::Idempotency(e.to_string()))?;
        Ok(())
    }
}

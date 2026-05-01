use async_trait::async_trait;
use bus_core::{
    error::BusError,
    id::MessageId,
    idempotency::{ClaimOutcome, IdempotencyStore},
};
use sqlx::PgPool;
use std::time::Duration;

/// PostgreSQL-backed idempotency store.
///
/// `try_claim` uses `INSERT … ON CONFLICT DO NOTHING RETURNING 1` to detect
/// first writes atomically. On conflict, a follow-up `SELECT status` reads the
/// existing row.
#[derive(Clone)]
pub struct PostgresIdempotencyStore {
    pool: PgPool,
    consumer: String,
}

impl PostgresIdempotencyStore {
    pub fn new(pool: PgPool, consumer: String) -> Self {
        Self { pool, consumer }
    }
}

#[async_trait]
impl IdempotencyStore for PostgresIdempotencyStore {
    async fn try_claim(
        &self,
        key: &MessageId,
        _ttl: Duration,
    ) -> Result<ClaimOutcome, BusError> {
        let inserted: Option<i32> = sqlx::query_scalar(
            r#"INSERT INTO eventbus_inbox (message_id, consumer, status)
               VALUES ($1, $2, 'pending')
               ON CONFLICT (consumer, message_id) DO NOTHING
               RETURNING 1"#,
        )
        .bind(key.to_string())
        .bind(&self.consumer)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| BusError::Idempotency(e.to_string()))?;

        if inserted.is_some() {
            return Ok(ClaimOutcome::Claimed);
        }

        let status: Option<String> = sqlx::query_scalar(
            "SELECT status FROM eventbus_inbox WHERE consumer = $1 AND message_id = $2",
        )
        .bind(&self.consumer)
        .bind(key.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| BusError::Idempotency(e.to_string()))?;

        match status.as_deref() {
            Some("pending") => Ok(ClaimOutcome::AlreadyPending),
            Some("done") => Ok(ClaimOutcome::AlreadyDone),
            Some(other) => Err(BusError::Idempotency(format!(
                "unexpected status `{other}` in eventbus_inbox"
            ))),
            None => Ok(ClaimOutcome::Claimed),
        }
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

    async fn release(&self, key: &MessageId) -> Result<(), BusError> {
        sqlx::query(
            "DELETE FROM eventbus_inbox WHERE consumer = $1 AND message_id = $2 AND status = 'pending'",
        )
        .bind(&self.consumer)
        .bind(key.to_string())
        .execute(&self.pool)
        .await
        .map_err(|e| BusError::Idempotency(e.to_string()))?;
        Ok(())
    }
}

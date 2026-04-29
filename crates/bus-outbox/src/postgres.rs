use crate::store::{OutboxRow, OutboxStore};
use async_trait::async_trait;
use bus_core::{error::BusError, event::Event, id::MessageId};
use sqlx::{PgPool, Postgres, Row, Transaction};

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
        tx: &mut Transaction<'tx, Postgres>,
        event: &E,
    ) -> Result<(), BusError> {
        let id = *event.message_id().as_uuid();
        let payload = serde_json::to_value(event).map_err(BusError::Serde)?;
        let aggregate_type = E::aggregate_type();
        let aggregate_id = event.message_id().to_string();
        let subject = event.subject().into_owned();

        sqlx::query(
            r#"INSERT INTO eventbus_outbox
               (id, aggregate_type, aggregate_id, subject, payload)
               VALUES ($1, $2, $3, $4, $5)"#,
        )
        .bind(id)
        .bind(aggregate_type)
        .bind(aggregate_id)
        .bind(subject)
        .bind(payload)
        .execute(&mut **tx)
        .await
        .map_err(|e| BusError::Outbox(e.to_string()))?;

        Ok(())
    }

    async fn fetch_pending(&self, limit: u32) -> Result<Vec<OutboxRow>, BusError> {
        let rows = sqlx::query(
            r#"SELECT id, subject, payload, headers, attempts
               FROM eventbus_outbox
               WHERE published_at IS NULL
               ORDER BY created_at
               LIMIT $1
               FOR UPDATE SKIP LOCKED"#,
        )
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| BusError::Outbox(e.to_string()))?;

        rows.into_iter()
            .map(|r| {
                Ok(OutboxRow {
                    id:       r.try_get("id").map_err(|e| BusError::Outbox(e.to_string()))?,
                    subject:  r.try_get("subject").map_err(|e| BusError::Outbox(e.to_string()))?,
                    payload:  r.try_get("payload").map_err(|e| BusError::Outbox(e.to_string()))?,
                    headers:  r.try_get("headers").map_err(|e| BusError::Outbox(e.to_string()))?,
                    attempts: r.try_get("attempts").map_err(|e| BusError::Outbox(e.to_string()))?,
                })
            })
            .collect()
    }

    async fn mark_published(&self, id: &MessageId) -> Result<(), BusError> {
        sqlx::query("UPDATE eventbus_outbox SET published_at = now() WHERE id = $1")
            .bind(id.as_uuid())
            .execute(&self.pool)
            .await
            .map_err(|e| BusError::Outbox(e.to_string()))?;
        Ok(())
    }

    async fn mark_failed(&self, id: &MessageId, error: &str) -> Result<(), BusError> {
        sqlx::query(
            "UPDATE eventbus_outbox SET attempts = attempts + 1, last_error = $2 WHERE id = $1",
        )
        .bind(id.as_uuid())
        .bind(error)
        .execute(&self.pool)
        .await
        .map_err(|e| BusError::Outbox(e.to_string()))?;
        Ok(())
    }
}

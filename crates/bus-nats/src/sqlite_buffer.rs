use rusqlite::{Connection, params};
use std::sync::{Arc, Mutex};

/// A row from the SQLite fallback buffer.
pub struct BufferRow {
    pub id:         String,
    pub subject:    String,
    pub payload:    Vec<u8>,
    pub headers:    String,
    pub created_at: i64,
    pub attempts:   i32,
}

/// Local SQLite buffer for storing events when NATS is unavailable.
/// Thread-safe via `Arc<Mutex<Connection>>`.
#[derive(Clone)]
pub struct SqliteBuffer {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteBuffer {
    /// Open or create a SQLite database at `path`.
    pub fn open(path: &str) -> Result<Self, rusqlite::Error> {
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    /// Create an in-memory SQLite database (useful for tests).
    pub fn in_memory() -> Result<Self, rusqlite::Error> {
        let conn = Connection::open_in_memory()?;
        Self::init(conn)
    }

    fn init(conn: Connection) -> Result<Self, rusqlite::Error> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS eventbus_buffer (
                id          TEXT    PRIMARY KEY,
                subject     TEXT    NOT NULL,
                payload     BLOB    NOT NULL,
                headers     TEXT    NOT NULL DEFAULT '{}',
                created_at  INTEGER NOT NULL,
                attempts    INTEGER NOT NULL DEFAULT 0
            );",
        )?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Insert an event into the buffer.
    pub fn insert(
        &self,
        id:         &str,
        subject:    &str,
        payload:    &[u8],
        headers:    &str,
        created_at: i64,
    ) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO eventbus_buffer (id, subject, payload, headers, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, subject, payload, headers, created_at],
        )?;
        Ok(())
    }

    /// Fetch up to `limit` rows ordered by `created_at` ASC (oldest first).
    pub fn fetch_pending(&self, limit: usize) -> Result<Vec<BufferRow>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, subject, payload, headers, created_at, attempts
             FROM eventbus_buffer
             ORDER BY created_at ASC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            Ok(BufferRow {
                id:         r.get(0)?,
                subject:    r.get(1)?,
                payload:    r.get(2)?,
                headers:    r.get(3)?,
                created_at: r.get(4)?,
                attempts:   r.get(5)?,
            })
        })?;
        rows.collect()
    }

    /// Delete a row by ID after successful relay.
    pub fn delete(&self, id: &str) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM eventbus_buffer WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Count pending rows (for metrics/monitoring).
    pub fn pending_count(&self) -> Result<i64, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT COUNT(*) FROM eventbus_buffer", [], |r| r.get(0))
    }
}

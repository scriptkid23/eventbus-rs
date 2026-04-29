#[cfg(feature = "postgres-outbox")]
pub mod dispatcher;
#[cfg(feature = "postgres-outbox")]
pub mod migrate;
#[cfg(feature = "postgres-outbox")]
pub mod postgres;
#[cfg(feature = "postgres-inbox")]
pub mod inbox_pg;
#[cfg(feature = "sqlite-buffer")]
pub mod sqlite;
pub mod store;

#[cfg(feature = "postgres-outbox")]
pub use postgres::PostgresOutboxStore;
#[cfg(feature = "postgres-inbox")]
pub use inbox_pg::PostgresIdempotencyStore;
#[cfg(feature = "sqlite-buffer")]
pub use sqlite::SqliteBuffer;
#[cfg(feature = "postgres-outbox")]
pub use store::{OutboxRow, OutboxStore};
